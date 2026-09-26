//! LAN end-to-end tests: pairing → transfer → resume (spec §103).
//!
//! Two nodes run in one process with `RelayConfig::Disabled` — proving the
//! offline-first guarantee (spec §7): no cloud, no relay, pure LAN path.
use std::path::PathBuf;
use std::time::Duration;

use dropbridge_core::config::NodeConfig;
use dropbridge_core::node::Node;
use dropbridge_core::pairing;
use dropbridge_core::session;
use dropbridge_protocol::DeviceKind;
use dropbridge_transfer::TransferSource;

fn base_cfg(dir: &std::path::Path, name: &str, kind: DeviceKind, port: u16) -> NodeConfig {
    let mut cfg = NodeConfig::new(dir.join(format!("{name}-state")), name.to_string(), kind);
    cfg.receive_dir = dir.join(format!("{name}-recv"));
    cfg.fixed_port = Some(port);
    cfg.relay = dropbridge_network::RelayConfig::Disabled;
    cfg.announce = false;
    cfg.auto_receive = true;
    cfg.pairing_auto_approve = true;
    cfg
}

async fn wait_trust_both(a: &Node, b: &Node) {
    for _ in 0..100 {
        let ab = a.trusted_devices().await;
        let ba = b.trusted_devices().await;
        if !ab.is_empty() && !ba.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("pairing did not converge");
}

fn make_tree(dir: &std::path::Path) -> PathBuf {
    let root = dir.join("Photos");
    std::fs::create_dir_all(root.join("2026")).unwrap();
    std::fs::write(root.join("a.jpg"), vec![0xAA; 4096]).unwrap();
    std::fs::write(root.join("2026/c.jpg"), vec![0xBB; 8192]).unwrap();
    std::fs::write(root.join("notes.txt"), "hello dropbridge").unwrap();
    root
}

async fn pair(a: &Node, b: &Node) {
    // A shows the QR; B scans it (we exercise the full QR string roundtrip).
    let inv = a.create_pair_invitation().await.unwrap();
    let qr = inv.to_qr_string().unwrap();
    let scanned = pairing::parse_qr(&qr).unwrap();
    b.join_pairing(scanned).await.unwrap();
    wait_trust_both(a, b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pair_then_transfer_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_a = base_cfg(tmp.path(), "laptop", DeviceKind::Laptop, 45001);
    let cfg_b = base_cfg(tmp.path(), "phone", DeviceKind::Phone, 45002);
    let node_a = Node::start(cfg_a.clone()).await.unwrap();
    let node_b = Node::start(cfg_b).await.unwrap();

    pair(&node_a, &node_b).await;

    let tree = make_tree(tmp.path());
    let a_id = node_a.device_id();
    let res = session::send_files(&node_b, &a_id, TransferSource::Paths(vec![tree]))
        .await
        .expect("send should succeed");
    assert!(res.ok);

    let recv = cfg_a.receive_dir.join("Photos");
    assert_eq!(std::fs::read(recv.join("a.jpg")).unwrap(), vec![0xAA; 4096]);
    assert_eq!(
        std::fs::read(recv.join("2026/c.jpg")).unwrap(),
        vec![0xBB; 8192]
    );
    assert_eq!(
        std::fs::read_to_string(recv.join("notes.txt")).unwrap(),
        "hello dropbridge"
    );
    // No part files must remain (spec §43).
    let leftovers: Vec<_> = walkdir_names(&cfg_a.receive_dir)
        .into_iter()
        .filter(|n| n.ends_with(".dropbridge-part"))
        .collect();
    assert!(leftovers.is_empty(), "leftover part files: {leftovers:?}");

    node_a.endpoint().close().await;
    node_b.endpoint().close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_injected_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg_a = base_cfg(tmp.path(), "laptop2", DeviceKind::Laptop, 45011);
    // Fault injection: fail after ~35% of a 2 MiB file (spec §83 chaos).
    cfg_a.test_recv_fail_after = Some(700_000);
    let cfg_b = base_cfg(tmp.path(), "phone2", DeviceKind::Phone, 45012);

    let node_a1 = Node::start(cfg_a.clone()).await.unwrap();
    let node_b = Node::start(cfg_b).await.unwrap();
    pair(&node_a1, &node_b).await;

    // 2 MiB of pseudo-random-ish data (compressibility irrelevant: no compression).
    let big = tmp.path().join("video.mp4");
    let data: Vec<u8> = (0..2 * 1024 * 1024u32)
        .map(|i| (i * 31 % 251) as u8)
        .collect();
    std::fs::write(&big, &data).unwrap();

    let session = 0x5E55_10F0u64;
    let a_id = node_a1.device_id();
    let first = session::send_files_with_session(
        &node_b,
        &a_id,
        TransferSource::Paths(vec![big.clone()]),
        Some(session),
    )
    .await;
    assert!(first.is_err(), "first attempt must fail via injected fault");

    // Receiver restarts (same state dir ⇒ journal survives).
    node_a1.stop().await;
    drop(node_a1);
    cfg_a.test_recv_fail_after = None;
    let node_a2 = Node::start(cfg_a.clone()).await.unwrap();

    // Retry with the SAME session → receiver answers with have_ranges →
    // sender skips completed ranges (resume near the breakpoint, spec §39).
    let res = session::send_files_with_session(
        &node_b,
        &a_id,
        TransferSource::Paths(vec![big.clone()]),
        Some(session),
    )
    .await
    .expect("resumed send should succeed");
    assert!(res.ok);

    let got = std::fs::read(cfg_a.receive_dir.join("video.mp4")).unwrap();
    assert_eq!(got, data, "resumed file must hash/byte-match");

    node_a2.endpoint().close().await;
    node_b.endpoint().close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn untrusted_sender_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_a = base_cfg(tmp.path(), "laptop3", DeviceKind::Laptop, 45021);
    let cfg_c = base_cfg(tmp.path(), "stranger", DeviceKind::Phone, 45022);
    let node_a = Node::start(cfg_a).await.unwrap();
    let node_c = Node::start(cfg_c).await.unwrap();
    // No pairing. Send must be refused by the sender-side trust gate.
    let f = tmp.path().join("x.txt");
    std::fs::write(&f, b"x").unwrap();
    let err = session::send_files(&node_c, &node_a.device_id(), TransferSource::Paths(vec![f]))
        .await
        .expect_err("must be refused");
    assert!(matches!(err, dropbridge_core::CoreError::NotTrusted));
    node_a.endpoint().close().await;
    node_c.endpoint().close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collision_renames_instead_of_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_a = base_cfg(tmp.path(), "laptop4", DeviceKind::Laptop, 45031);
    let cfg_b = base_cfg(tmp.path(), "phone4", DeviceKind::Phone, 45032);
    let node_a = Node::start(cfg_a.clone()).await.unwrap();
    let node_b = Node::start(cfg_b).await.unwrap();
    pair(&node_a, &node_b).await;

    let f = tmp.path().join("photo.jpg");
    std::fs::write(&f, b"first").unwrap();
    let a_id = node_a.device_id();
    let r1 = session::send_files(&node_b, &a_id, TransferSource::Paths(vec![f.clone()]))
        .await
        .unwrap();
    assert!(r1.ok);

    std::fs::write(&f, b"second").unwrap();
    let r2 = session::send_files(&node_b, &a_id, TransferSource::Paths(vec![f.clone()]))
        .await
        .unwrap();
    assert!(r2.ok);

    assert_eq!(
        std::fs::read(cfg_a.receive_dir.join("photo.jpg")).unwrap(),
        b"first"
    );
    assert_eq!(
        std::fs::read(cfg_a.receive_dir.join("photo (1).jpg")).unwrap(),
        b"second"
    );
    node_a.endpoint().close().await;
    node_b.endpoint().close().await;
}

/// Tiny recursive walker (avoids a walkdir dependency).
fn walkdir_names(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    fn rec(p: &std::path::Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(p) else { return };
        for e in rd.flatten() {
            let path = e.path();
            out.push(
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
            if path.is_dir() {
                rec(&path, out);
            }
        }
    }
    rec(root, &mut out);
    out
}
