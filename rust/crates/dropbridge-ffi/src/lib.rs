//! dropbridge-ffi — the C-ABI surface for Flutter/Kotlin/Swift hosts.
//!
//! Design rule (spec §26): **only commands, metadata and events cross this
//! boundary — never file bytes.** Transfers stream inside the Rust core.
//!
//! Every function returns either an opaque handle, a JSON C-string (owned,
//! free with [`db_free_string`]), or an int status. Errors are stored
//! thread-locally and readable via [`db_last_error`].
//!
//! JSON shapes are stable-ish and versioned with a `"v":1` field.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dropbridge_core::config::NodeConfig;
use dropbridge_core::events::NodeEvent;
use dropbridge_core::node::Node;
use dropbridge_core::{pairing, session, watcher};
use dropbridge_protocol::DeviceKind;
use tokio::runtime::Runtime;
use tokio::sync::broadcast;

thread_local! {
    static LAST_ERROR: std::cell::RefCell<Option<CString>> =
        const { std::cell::RefCell::new(None) };
}

fn clear_error() {
    LAST_ERROR.with(|c| *c.borrow_mut() = None);
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn set_error(e: impl std::fmt::Display) {
    let msg = CString::new(format!("{e}")).unwrap_or_default();
    LAST_ERROR.with(|c| *c.borrow_mut() = Some(msg));
}

unsafe fn cstr<'a>(p: *const c_char) -> Result<&'a str, ()> {
    if p.is_null() {
        set_error("null string argument");
        return Err(());
    }
    match CStr::from_ptr(p).to_str() {
        Ok(s) => Ok(s),
        Err(_) => {
            set_error("argument is not valid UTF-8");
            Err(())
        }
    }
}

fn out_json(v: serde_json::Value) -> *mut c_char {
    CString::new(v.to_string())
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

pub struct DbHandle {
    inner: Arc<DbHandleInner>,
}

pub struct DbHandleInner {
    rt: Arc<Runtime>,
    node: Arc<Node>,
    events: tokio::sync::Mutex<broadcast::Receiver<NodeEvent>>,
    watchers: std::sync::Mutex<Vec<watcher::OutboxHandle>>,
    closed: AtomicBool,
}

fn parse_kind(s: &str) -> DeviceKind {
    match s.to_ascii_lowercase().as_str() {
        "phone" => DeviceKind::Phone,
        "tablet" => DeviceKind::Tablet,
        "desktop" => DeviceKind::Desktop,
        _ => DeviceKind::Laptop,
    }
}

fn parse_relay(s: &str) -> dropbridge_network::RelayConfig {
    match s {
        "disabled" | "lan" => dropbridge_network::RelayConfig::Disabled,
        "n0" | "default" | "" => dropbridge_network::RelayConfig::N0Default,
        other => dropbridge_network::RelayConfig::Custom(
            other
                .trim_start_matches("urls=")
                .split(',')
                .map(|u| u.trim().to_string())
                .filter(|u| !u.is_empty())
                .collect(),
        ),
    }
}

/// Create and start a node. `cfg_json`:
/// `{"state_dir","receive_dir","name","kind","relay","port"?,
///    "announce"?,"auto_receive"?,"pairing_auto_approve"?}`
///
/// # Safety
/// `cfg_json` must be a valid NUL-terminated UTF-8 string.
/// Internal implementation for initializing a node with optional hardware key seed.
unsafe fn init_internal(cfg_json: *const c_char, hardware_seed: Option<[u8; 32]>) -> *mut DbHandle {
    let res = std::panic::catch_unwind(|| -> Result<*mut DbHandle, String> {
        let s = cstr(cfg_json).map_err(|_| "bad cfg".to_string())?;
        let v: serde_json::Value = serde_json::from_str(s).map_err(|e| format!("cfg json: {e}"))?;
        let get = |k: &str| -> Result<String, String> {
            v.get(k)
                .and_then(|x| x.as_str())
                .map(|x| x.to_string())
                .ok_or_else(|| format!("missing cfg field {k:?}"))
        };
        if let Some(seed) = hardware_seed {
            if seed.iter().all(|&b| b == 0) || seed.iter().all(|&b| b == 0xff) {
                return Err("weak or invalid hardware key seed".to_string());
            }
        }
        let mut cfg = NodeConfig::new(
            PathBuf::from(get("state_dir")?),
            get("name")?,
            parse_kind(v.get("kind").and_then(|x| x.as_str()).unwrap_or("laptop")),
        );
        cfg.receive_dir = PathBuf::from(get("receive_dir")?);
        cfg.relay = parse_relay(v.get("relay").and_then(|x| x.as_str()).unwrap_or("n0"));
        if let Some(p) = v.get("port").and_then(|x| x.as_u64()) {
            cfg.fixed_port = Some(p as u16);
        }
        if let Some(b) = v.get("announce").and_then(|x| x.as_bool()) {
            cfg.announce = b;
        }
        if let Some(b) = v.get("auto_receive").and_then(|x| x.as_bool()) {
            cfg.auto_receive = b;
        }
        if let Some(b) = v.get("pairing_auto_approve").and_then(|x| x.as_bool()) {
            cfg.pairing_auto_approve = b;
        }
        cfg.hardware_identity_seed = hardware_seed;

        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("dropbridge-ffi")
                .build()
                .map_err(|e| format!("runtime: {e}"))?,
        );
        let node = rt
            .block_on(Node::start(cfg))
            .map_err(|e| format!("node start: {e}"))?;
        let events = tokio::sync::Mutex::new(node.events());
        let watchers = std::sync::Mutex::new(Vec::new());
        let closed = AtomicBool::new(false);
        let inner = Arc::new(DbHandleInner {
            rt,
            node,
            events,
            watchers,
            closed,
        });
        Ok(Box::into_raw(Box::new(DbHandle { inner })))
    });
    match res {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => {
            set_error(e);
            std::ptr::null_mut()
        }
        Err(_) => {
            set_error("panic in db_init");
            std::ptr::null_mut()
        }
    }
}

/// Create and start a node. `cfg_json`:
/// `{"state_dir","receive_dir","name","kind","relay","port"?,
///    "announce"?,"auto_receive"?,"pairing_auto_approve"?}`
///
/// # Safety
/// `cfg_json` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn db_init(cfg_json: *const c_char) -> *mut DbHandle {
    init_internal(cfg_json, None)
}

/// Create and start a node with an in-memory 32-byte hardware-unsealed identity key.
/// Used by Android Keystore / TEE and secure platform hosts.
///
/// # Safety
/// `cfg_json` must be a valid NUL-terminated UTF-8 string, and `key_seed` must point to 32 bytes.
#[no_mangle]
pub unsafe extern "C" fn db_init_with_key(
    cfg_json: *const c_char,
    key_seed: *const u8,
) -> *mut DbHandle {
    if key_seed.is_null() {
        set_error("null key_seed argument");
        return std::ptr::null_mut();
    }
    let seed_slice = std::slice::from_raw_parts(key_seed, 32);
    let mut seed = [0u8; 32];
    seed.copy_from_slice(seed_slice);
    init_internal(cfg_json, Some(seed))
}

/// JNI export for direct invocation from Android Kotlin / Java.
///
/// # Safety
/// Standard JNI calling convention with `cfg_json` and `key_seed` pointers.
#[no_mangle]
pub unsafe extern "C" fn Java_app_dropbridge_app_DropBridgeNative_initNodeWithKey(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    cfg_json: *const c_char,
    key_seed: *const u8,
) -> *mut DbHandle {
    db_init_with_key(cfg_json, key_seed)
}

unsafe fn with_handle<F, T>(h: *mut DbHandle, f: F) -> Option<T>
where
    F: FnOnce(&DbHandleInner) -> Result<T, String>,
{
    clear_error();
    if h.is_null() {
        set_error("null handle");
        return None;
    }
    let handle = &*h;
    let inner = Arc::clone(&handle.inner);
    if inner.closed.load(Ordering::Acquire) {
        set_error("handle is closed / shut down");
        return None;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&inner))) {
        Ok(Ok(v)) => Some(v),
        Ok(Err(e)) => {
            set_error(e);
            None
        }
        Err(_) => {
            set_error("panic inside FFI call");
            None
        }
    }
}

/// This device's id + name. Returns JSON.
///
/// # Safety
/// Handle must come from [`db_init`].
#[no_mangle]
pub unsafe extern "C" fn db_info(h: *mut DbHandle) -> *mut c_char {
    with_handle(h, |h| {
        Ok(out_json(serde_json::json!({
            "v": 1,
            "device_id": dropbridge_network::hints::id_z32(&h.node.device_id()),
            "name": h.node.config().device_name,
        })))
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Create a pairing invitation; returns `{"qr": "dropbridge://pair?..."}`.
///
/// # Safety
/// Handle must come from [`db_init`].
#[no_mangle]
pub unsafe extern "C" fn db_pair_qr(h: *mut DbHandle) -> *mut c_char {
    with_handle(h, |h| {
        let inv =
            h.rt.block_on(h.node.create_pair_invitation())
                .map_err(|e| format!("pair: {e}"))?;
        let qr = inv.to_qr_string().map_err(|e| format!("qr: {e}"))?;
        Ok(out_json(serde_json::json!({ "v": 1, "qr": qr })))
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Join using a scanned QR string. 0 = ok.
///
/// # Safety
/// Handle must come from [`db_init`]; `qr` must be valid UTF-8.
#[no_mangle]
pub unsafe extern "C" fn db_join(h: *mut DbHandle, qr: *const c_char) -> c_int {
    let qr = match cstr(qr) {
        Ok(s) => s.to_string(),
        Err(_) => return -1,
    };
    with_handle(h, |h| {
        let inv = pairing::parse_qr(&qr).map_err(|e| format!("qr parse: {e}"))?;
        h.rt.block_on(h.node.join_pairing(inv))
            .map_err(|e| format!("join: {e}"))
    })
    .map(|_| 0)
    .unwrap_or(-1)
}

/// Trusted devices as JSON array.
///
/// # Safety
/// Handle must come from [`db_init`].
#[no_mangle]
pub unsafe extern "C" fn db_devices(h: *mut DbHandle) -> *mut c_char {
    with_handle(h, |h| {
        let devices = h.rt.block_on(h.node.trusted_devices());
        Ok(out_json(serde_json::json!({ "v": 1, "devices": devices })))
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Send paths to a device. `send_json`:
/// `{"peer": "<name-or-id-prefix>", "paths": ["..."], "session": null|u64}`
/// Returns `{"session": n, "ok": bool, "bytes": n, "detail": "..."}`.
/// Blocks until the transfer completes (call from a background isolate/thread).
///
/// # Safety
/// Handle must come from [`db_init`]; `send_json` must be valid UTF-8.
#[no_mangle]
pub unsafe extern "C" fn db_send(h: *mut DbHandle, send_json: *const c_char) -> *mut c_char {
    let arg = match cstr(send_json) {
        Ok(s) => s.to_string(),
        Err(_) => return std::ptr::null_mut(),
    };
    with_handle(h, |h| {
        let v: serde_json::Value = serde_json::from_str(&arg).map_err(|e| format!("json: {e}"))?;
        let peer_raw = v
            .get("peer")
            .and_then(|x| x.as_str())
            .ok_or("missing peer")?;
        let peer = peer_raw.trim().to_string();
        if peer.is_empty() {
            return Err("peer identifier cannot be empty".to_string());
        }
        let source = if let Some(fds_arr) = v.get("fds").and_then(|x| x.as_array()) {
            let mut fds = Vec::new();
            for f in fds_arr {
                let name = f
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("file")
                    .to_string();
                let size = f.get("size").and_then(|x| x.as_u64()).unwrap_or(0);
                let fd = f.get("fd").and_then(|x| x.as_i64()).ok_or("missing fd")? as i32;
                let mtime = f
                    .get("mtime")
                    .and_then(|x| x.as_i64())
                    .unwrap_or_else(now_unix);
                fds.push(dropbridge_transfer::FdSource {
                    name,
                    size,
                    mtime_secs: mtime,
                    fd,
                });
            }
            if fds.is_empty() {
                return Err("empty fds".into());
            }
            dropbridge_transfer::TransferSource::Fds(fds)
        } else {
            let paths: Vec<PathBuf> = v
                .get("paths")
                .and_then(|x| x.as_array())
                .ok_or("missing paths or fds")?
                .iter()
                .filter_map(|x| x.as_str())
                .map(PathBuf::from)
                .collect();
            if paths.is_empty() {
                return Err("empty paths".into());
            }
            dropbridge_transfer::TransferSource::Paths(paths)
        };
        let session_override = v.get("session").and_then(|x| x.as_u64());

        let node = h.node.clone();
        let res = h.rt.block_on(async move {
            let devices = node.trusted_devices().await;
            let q = peer.to_lowercase();
            let mut target: Vec<_> = devices
                .iter()
                .filter(|d| {
                    d.name.eq_ignore_ascii_case(&peer)
                        || dropbridge_network::hints::id_z32(&d.device_id)
                            .eq_ignore_ascii_case(&peer)
                })
                .cloned()
                .collect();
            if target.is_empty() {
                if q.len() < 3 {
                    return Err(format!(
                        "peer query {peer:?} is too short; minimum 3 characters required"
                    ));
                }
                target = devices
                    .into_iter()
                    .filter(|d| {
                        d.name.to_lowercase().contains(&q)
                            || dropbridge_network::hints::id_z32(&d.device_id)
                                .to_lowercase()
                                .starts_with(&q)
                    })
                    .collect();
            }
            if target.is_empty() {
                return Err(format!("no trusted device matches {peer:?}"));
            }
            if target.len() > 1 {
                return Err(format!("multiple devices match {peer:?}; be more specific"));
            }
            let peer_id = target[0].device_id;
            if node.hints_for(&peer_id).await.is_none() {
                let _ = node.discover(Duration::from_secs(4)).await;
            }
            match session_override {
                Some(sid) => {
                    session::send_files_with_session(&node, &peer_id, source, Some(sid)).await
                }
                None => session::send_files(&node, &peer_id, source).await,
            }
            .map_err(|e| format!("send: {e}"))
        })?;
        Ok(out_json(serde_json::json!({
            "v": 1,
            "session": res.session,
            "ok": res.ok,
            "bytes": res.bytes,
            "detail": res.detail,
        })))
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Send a native file descriptor directly (Android ContentResolver / ParcelFileDescriptor.detachFd()).
/// Returns JSON transfer result.
///
/// # Safety
/// Handle must come from [`db_init`].
#[no_mangle]
pub unsafe extern "C" fn db_send_fd(
    h: *mut DbHandle,
    peer: *const c_char,
    name: *const c_char,
    fd: c_int,
    size: u64,
) -> *mut c_char {
    let peer_str = match cstr(peer) {
        Ok(s) => s.to_string(),
        Err(_) => return std::ptr::null_mut(),
    };
    let name_str = match cstr(name) {
        Ok(s) => s.to_string(),
        Err(_) => return std::ptr::null_mut(),
    };
    let json_req = serde_json::json!({
        "peer": peer_str,
        "fds": [{
            "name": name_str,
            "fd": fd,
            "size": size,
            "mtime": now_unix(),
        }]
    });
    let c_json = match CString::new(json_req.to_string()) {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };
    db_send(h, c_json.as_ptr())
}

/// Start the outbox watcher (files dropped into `outbox` go to the best
/// trusted phone). 0 = ok.
///
/// # Safety
/// Handle must come from [`db_init`]; `outbox` must be valid UTF-8.
#[no_mangle]
pub unsafe extern "C" fn db_start_watcher(h: *mut DbHandle, outbox: *const c_char) -> c_int {
    let outbox = match cstr(outbox) {
        Ok(s) => PathBuf::from(s),
        Err(_) => return -1,
    };
    with_handle(h, |h| {
        let w = watcher::spawn_outbox_watcher(h.node.clone(), outbox, watcher::OutboxTarget::Auto)
            .map_err(|e| format!("watcher: {e}"))?;
        let mut watchers = h
            .watchers
            .lock()
            .map_err(|e| format!("mutex poisoned: {e}"))?;
        watchers.push(w);
        Ok(0)
    })
    .unwrap_or(-1)
}

/// Add a directory to the continuously synchronized folders list.
/// `folder_json`: `{"path": "/path/to/folder", "target": "auto"|"<device-id>"}`
/// Returns 0 on success, -1 on error.
///
/// # Safety
/// Handle must come from [`db_init`]; `folder_json` must be valid UTF-8.
#[no_mangle]
pub unsafe extern "C" fn db_sync_folder_add(h: *mut DbHandle, folder_json: *const c_char) -> c_int {
    let arg = match cstr(folder_json) {
        Ok(s) => s.to_string(),
        Err(_) => return -1,
    };
    with_handle(h, |h| {
        let v: serde_json::Value = serde_json::from_str(&arg).map_err(|e| format!("json: {e}"))?;
        let path_str = v
            .get("path")
            .and_then(|x| x.as_str())
            .ok_or("missing path")?;
        let path = PathBuf::from(path_str);
        let target_str = v.get("target").and_then(|x| x.as_str()).unwrap_or("auto");
        let target = if target_str.eq_ignore_ascii_case("auto") {
            watcher::OutboxTarget::Auto
        } else {
            watcher::OutboxTarget::Device(target_str.to_string())
        };
        h.rt.block_on(h.node.add_sync_folder(path, target))
            .map_err(|e| format!("add_sync_folder: {e}"))?;
        Ok(0)
    })
    .unwrap_or(-1)
}

/// Remove a directory from continuously synchronized folders.
/// Returns 0 on success, -1 on error.
///
/// # Safety
/// Handle must come from [`db_init`]; `path` must be valid UTF-8.
#[no_mangle]
pub unsafe extern "C" fn db_sync_folder_remove(h: *mut DbHandle, path: *const c_char) -> c_int {
    let path = match cstr(path) {
        Ok(s) => PathBuf::from(s),
        Err(_) => return -1,
    };
    with_handle(h, |h| {
        h.rt.block_on(h.node.remove_sync_folder(&path))
            .map_err(|e| format!("remove_sync_folder: {e}"))?;
        Ok(0)
    })
    .unwrap_or(-1)
}

/// List all configured sync folders as JSON array:
/// `{"v": 1, "folders": [{"path": "...", "target": "auto", "enabled": true}]}`.
///
/// # Safety
/// Handle must come from [`db_init`].
#[no_mangle]
pub unsafe extern "C" fn db_sync_folders_list(h: *mut DbHandle) -> *mut c_char {
    with_handle(h, |h| {
        let folders = h.node.list_sync_folders();
        let list: Vec<serde_json::Value> = folders
            .into_iter()
            .map(|f| {
                let tgt = match f.target {
                    watcher::OutboxTarget::Auto => "auto".to_string(),
                    watcher::OutboxTarget::Device(d) => d,
                };
                serde_json::json!({
                    "path": f.path.to_string_lossy(),
                    "target": tgt,
                    "enabled": f.enabled,
                })
            })
            .collect();
        Ok(out_json(serde_json::json!({ "v": 1, "folders": list })))
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Wait up to `timeout_ms` for the next node event. Returns JSON or null on
/// timeout. Long-poll this from the UI thread/isolate.
///
/// # Safety
/// Handle must come from [`db_init`].
#[no_mangle]
pub unsafe extern "C" fn db_event(h: *mut DbHandle, timeout_ms: u32) -> *mut c_char {
    with_handle(h, |h| {
        let ev = h.rt.block_on(async {
            let mut rx = h.events.lock().await;
            tokio::time::timeout(Duration::from_millis(timeout_ms as u64), rx.recv()).await
        });
        let ev = match ev {
            Err(_) => return Ok(std::ptr::null_mut()), // timeout = no event yet
            Ok(e) => e,
        };
        match ev {
            Ok(e) => {
                let mut v = serde_json::to_value(&e).map_err(|er| format!("ser: {er}"))?;
                if let Some(o) = v.as_object_mut() {
                    o.insert("v".into(), serde_json::json!(1));
                }
                Ok(out_json(v))
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                Ok(out_json(serde_json::json!({"v":1, "type":"lagged"})))
            }
            Err(broadcast::error::RecvError::Closed) => Err("event channel closed".into()),
        }
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Stop the node and free the handle. The handle is invalid afterwards.
///
/// # Safety
/// Handle must come from [`db_init`] and not previously shut down.
#[no_mangle]
pub unsafe extern "C" fn db_shutdown(h: *mut DbHandle) {
    if h.is_null() {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let handle = Box::from_raw(h);
        if handle.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(mut watchers) = handle.inner.watchers.lock() {
            watchers.clear();
        }
        let rt = handle.inner.rt.clone();
        let node = handle.inner.node.clone();
        rt.block_on(async {
            node.close().await;
        });
        // handle is dropped here, freeing the outer wrapper while any active
        // call holding inner through Arc completes cleanly without SIGSEGV.
    }));
}

/// Cancel an ongoing transfer session by session id.
/// Returns 0 on success, -1 on error or if no active transfer matches.
///
/// # Safety
/// Handle must come from [`db_init`].
#[no_mangle]
pub unsafe extern "C" fn db_cancel(h: *mut DbHandle, session: u64) -> c_int {
    with_handle(h, |inner| {
        let ok = inner.rt.block_on(inner.node.cancel_transfer(session));
        if ok {
            Ok(0)
        } else {
            Err(format!("no active transfer session {session} to cancel"))
        }
    })
    .unwrap_or(-1)
}

/// Last error on this thread ("" if none). Pointer is owned by the library
/// until the next failing call on the same thread.
#[no_mangle]
pub extern "C" fn db_last_error() -> *const c_char {
    LAST_ERROR.with(|c| {
        c.borrow()
            .as_ref()
            .map(|s| s.as_ptr())
            .unwrap_or_else(|| EMPTY.as_ptr() as *const c_char)
    })
}

static EMPTY: &[u8] = b"\0";

/// Free a string returned by this library.
///
/// # Safety
/// Must be a pointer returned by a `db_*` function, once.
#[no_mangle]
pub unsafe extern "C" fn db_free_string(s: *mut c_char) {
    if !s.is_null() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(CString::from_raw(s));
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn test_null_pointer_safety() {
        unsafe {
            assert_eq!(db_join(std::ptr::null_mut(), std::ptr::null()), -1);
            assert!(db_info(std::ptr::null_mut()).is_null());
            assert!(db_pair_qr(std::ptr::null_mut()).is_null());
            assert!(db_devices(std::ptr::null_mut()).is_null());
            assert!(db_send(std::ptr::null_mut(), std::ptr::null()).is_null());
            assert!(db_send_fd(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                0
            )
            .is_null());
            assert_eq!(db_start_watcher(std::ptr::null_mut(), std::ptr::null()), -1);
            assert_eq!(
                db_sync_folder_add(std::ptr::null_mut(), std::ptr::null()),
                -1
            );
            assert_eq!(
                db_sync_folder_remove(std::ptr::null_mut(), std::ptr::null()),
                -1
            );
            assert!(db_sync_folders_list(std::ptr::null_mut()).is_null());
            assert!(db_event(std::ptr::null_mut(), 10).is_null());
            assert_eq!(db_cancel(std::ptr::null_mut(), 0), -1);
            db_shutdown(std::ptr::null_mut());
            db_free_string(std::ptr::null_mut());
        }
    }

    #[test]
    fn test_init_invalid_json() {
        unsafe {
            let bad_json = CString::new("not valid json").unwrap();
            let handle = db_init(bad_json.as_ptr());
            assert!(handle.is_null());
            let err_ptr = db_last_error();
            assert!(!err_ptr.is_null());
            let err = CStr::from_ptr(err_ptr).to_str().unwrap();
            assert!(err.contains("cfg json"));
        }
    }

    #[test]
    fn test_weak_hardware_seeds_rejected() {
        unsafe {
            let tmp = tempfile::tempdir().unwrap();
            let cfg = serde_json::json!({
                "state_dir": tmp.path().to_str().unwrap(),
                "receive_dir": tmp.path().to_str().unwrap(),
                "name": "WeakTest",
                "relay": "disabled",
            });
            let cfg_c = CString::new(cfg.to_string()).unwrap();

            // All zeroes
            let zeroes = [0u8; 32];
            let h = db_init_with_key(cfg_c.as_ptr(), zeroes.as_ptr());
            assert!(h.is_null());
            let err = CStr::from_ptr(db_last_error()).to_str().unwrap();
            assert!(err.contains("weak or invalid"));

            // All 0xff
            let all_ones = [0xffu8; 32];
            let h = db_init_with_key(cfg_c.as_ptr(), all_ones.as_ptr());
            assert!(h.is_null());

            // Null pointer seed
            let h = db_init_with_key(cfg_c.as_ptr(), std::ptr::null());
            assert!(h.is_null());
        }
    }

    #[test]
    fn test_lifecycle_and_basic_calls() {
        unsafe {
            let tmp = tempfile::tempdir().unwrap();
            let cfg = serde_json::json!({
                "state_dir": tmp.path().join("state").to_str().unwrap(),
                "receive_dir": tmp.path().join("recv").to_str().unwrap(),
                "name": "LifecycleNode",
                "relay": "disabled",
                "announce": false,
            });
            let cfg_c = CString::new(cfg.to_string()).unwrap();
            let h = db_init(cfg_c.as_ptr());
            assert!(!h.is_null());

            // db_info
            let info_p = db_info(h);
            assert!(!info_p.is_null());
            let info_s = CStr::from_ptr(info_p).to_str().unwrap();
            let info_v: serde_json::Value = serde_json::from_str(info_s).unwrap();
            assert_eq!(info_v["name"], "LifecycleNode");
            assert!(info_v["device_id"].is_string());
            db_free_string(info_p);

            // db_pair_qr
            let qr_p = db_pair_qr(h);
            assert!(!qr_p.is_null());
            let qr_s = CStr::from_ptr(qr_p).to_str().unwrap();
            let qr_v: serde_json::Value = serde_json::from_str(qr_s).unwrap();
            assert!(qr_v["qr"]
                .as_str()
                .unwrap()
                .starts_with("dropbridge://pair"));
            db_free_string(qr_p);

            // db_devices
            let dev_p = db_devices(h);
            assert!(!dev_p.is_null());
            let dev_s = CStr::from_ptr(dev_p).to_str().unwrap();
            let dev_v: serde_json::Value = serde_json::from_str(dev_s).unwrap();
            assert!(dev_v["devices"].is_array());
            db_free_string(dev_p);

            // db_event with short timeout returns null (timeout)
            let ev_p = db_event(h, 20);
            assert!(ev_p.is_null());

            // db_send rejects empty peer
            let send_empty_peer = serde_json::json!({
                "peer": "   ",
                "paths": ["test.txt"]
            });
            let send_empty_c = CString::new(send_empty_peer.to_string()).unwrap();
            let send_res = db_send(h, send_empty_c.as_ptr());
            assert!(send_res.is_null());
            let err = CStr::from_ptr(db_last_error()).to_str().unwrap();
            assert!(err.contains("empty"));

            // db_sync_folders_list initial (empty)
            let list_p = db_sync_folders_list(h);
            assert!(!list_p.is_null());
            let list_s = CStr::from_ptr(list_p).to_str().unwrap();
            let list_v: serde_json::Value = serde_json::from_str(list_s).unwrap();
            assert_eq!(list_v["folders"].as_array().unwrap().len(), 0);
            db_free_string(list_p);

            // db_sync_folder_add
            let sync_test_dir = tmp.path().join("ffi_sync_dir");
            let add_req = serde_json::json!({
                "path": sync_test_dir.to_str().unwrap(),
                "target": "auto"
            });
            let add_c = CString::new(add_req.to_string()).unwrap();
            assert_eq!(db_sync_folder_add(h, add_c.as_ptr()), 0);

            // db_sync_folders_list after add (contains 1)
            let list_p2 = db_sync_folders_list(h);
            assert!(!list_p2.is_null());
            let list_s2 = CStr::from_ptr(list_p2).to_str().unwrap();
            let list_v2: serde_json::Value = serde_json::from_str(list_s2).unwrap();
            assert_eq!(list_v2["folders"].as_array().unwrap().len(), 1);
            db_free_string(list_p2);

            // db_sync_folder_remove
            let rem_c = CString::new(sync_test_dir.to_str().unwrap()).unwrap();
            assert_eq!(db_sync_folder_remove(h, rem_c.as_ptr()), 0);

            // db_sync_folders_list after remove (empty again)
            let list_p3 = db_sync_folders_list(h);
            assert!(!list_p3.is_null());
            let list_s3 = CStr::from_ptr(list_p3).to_str().unwrap();
            let list_v3: serde_json::Value = serde_json::from_str(list_s3).unwrap();
            assert_eq!(list_v3["folders"].as_array().unwrap().len(), 0);
            db_free_string(list_p3);

            // db_shutdown
            db_shutdown(h);
        }
    }
}
