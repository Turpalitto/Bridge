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
    rt: Arc<Runtime>,
    node: Arc<Node>,
    events: broadcast::Receiver<NodeEvent>,
    watchers: Vec<watcher::OutboxHandle>,
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
        let events = node.events();
        Ok(Box::into_raw(Box::new(DbHandle {
            rt,
            node,
            events,
            watchers: Vec::new(),
        })))
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

unsafe fn with_handle<F, T>(h: *mut DbHandle, f: F) -> T
where
    F: FnOnce(&mut DbHandle) -> Result<T, String>,
    T: Default,
{
    if h.is_null() {
        set_error("null handle");
        return T::default();
    }
    let handle = &mut *h;
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(handle))) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            set_error(e);
            T::default()
        }
        Err(_) => {
            set_error("panic inside FFI call");
            T::default()
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
}

/// Join using a scanned QR string. 0 = ok.
///
/// # Safety
/// Handle must come from [`db_init`]; `qr` must be valid UTF-8.
#[no_mangle]
pub unsafe extern "C" fn db_join(h: *mut DbHandle, qr: *const c_char) -> c_int {
    if h.is_null() {
        set_error("null handle");
        return -1;
    }
    let qr = match cstr(qr) {
        Ok(s) => s.to_string(),
        Err(_) => return -1,
    };
    let handle = &mut *h;
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let inv = pairing::parse_qr(&qr).map_err(|e| format!("qr parse: {e}"))?;
        handle
            .rt
            .block_on(handle.node.join_pairing(inv))
            .map_err(|e| format!("join: {e}"))
    }));
    match res {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            set_error(e);
            -1
        }
        Err(_) => {
            set_error("panic in db_join");
            -1
        }
    }
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
        let peer = v
            .get("peer")
            .and_then(|x| x.as_str())
            .ok_or("missing peer")?
            .to_string();
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
            let target = devices
                .into_iter()
                .filter(|d| {
                    d.name.to_lowercase().contains(&q)
                        || dropbridge_network::hints::id_z32(&d.device_id)
                            .to_lowercase()
                            .starts_with(&q)
                })
                .collect::<Vec<_>>();
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
    if h.is_null() {
        set_error("null handle");
        return -1;
    }
    let outbox = match cstr(outbox) {
        Ok(s) => PathBuf::from(s),
        Err(_) => return -1,
    };
    let handle = &mut *h;
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        watcher::spawn_outbox_watcher(handle.node.clone(), outbox, watcher::OutboxTarget::Auto)
            .map_err(|e| format!("watcher: {e}"))
    }));
    match res {
        Ok(Ok(w)) => {
            handle.watchers.push(w);
            0
        }
        Ok(Err(e)) => {
            set_error(e);
            -1
        }
        Err(_) => {
            set_error("panic in db_start_watcher");
            -1
        }
    }
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
            tokio::time::timeout(Duration::from_millis(timeout_ms as u64), h.events.recv()).await
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
    let boxed = Box::from_raw(h);
    boxed.rt.block_on(boxed.node.endpoint().close());
    // Runtime drops after node; background tasks end with the runtime.
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
        drop(CString::from_raw(s));
    }
}
