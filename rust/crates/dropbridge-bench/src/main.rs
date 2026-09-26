//! dropbridge-bench — honest numbers, no marketing (spec §64–67, §105).
//!
//! Measures: disk write/read, BLAKE3 hashing, full loopback DropBridge
//! transfer (pair → offer → chunk streams → verify), stream-count matrix,
//! and zstd compressibility samples. Results are printed as Markdown so CI
//! can embed them into the run summary and docs/BENCHMARKS.md.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use dropbridge_core::config::NodeConfig;
use dropbridge_core::node::Node;
use dropbridge_core::pairing;
use dropbridge_core::session;
use dropbridge_protocol::DeviceKind;
use dropbridge_transfer::TransferSource;

#[derive(Parser)]
#[command(
    name = "dropbridge-bench",
    version,
    about = "DropBridge benchmark suite"
)]
struct Cli {
    /// Quick profile: smaller files, no matrix (CI default).
    #[arg(long)]
    quick: bool,
    /// Directory for bench files.
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Run the stream-count matrix.
    #[arg(long)]
    matrix: bool,
}

fn mbps(bytes: u64, d: Duration) -> f64 {
    let secs = d.as_secs_f64().max(1e-9);
    (bytes as f64 / secs) / 1_000_000.0 * 8.0 // megabits
}

fn mibs(bytes: u64, d: Duration) -> f64 {
    let secs = d.as_secs_f64().max(1e-9);
    (bytes as f64 / secs) / (1024.0 * 1024.0)
}

fn write_test_file(path: &Path, size: usize) -> Result<()> {
    // Deterministic pseudo-compressible content (like a mixed media file).
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut written = 0usize;
    let mut i = 0u64;
    while written < size {
        for (j, b) in buf.iter_mut().enumerate() {
            *b = ((i as usize + j) * 31 % 251) as u8;
        }
        i += 1;
        let n = buf.len().min(size - written);
        f.write_all(&buf[..n])?;
        written += n;
    }
    Ok(())
}

async fn disk_bench(dir: &Path, size_mb: usize) -> Result<()> {
    let path = dir.join(format!("disk-{size_mb}mb.bin"));
    let size = size_mb * 1024 * 1024;

    let t = Instant::now();
    write_test_file(&path, size)?;
    let w = t.elapsed();

    let t = Instant::now();
    let hash = dropbridge_transfer::hash::hash_file(&path).await?;
    let h = t.elapsed();

    let t = Instant::now();
    let read = tokio::task::spawn_blocking({
        let path = path.clone();
        move || {
            use std::io::Read;
            let mut f = std::fs::File::open(&path)?;
            let mut buf = vec![0u8; 1024 * 1024];
            let mut total = 0usize;
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                total += n;
            }
            Ok::<usize, std::io::Error>(total)
        }
    })
    .await??;
    let r = t.elapsed();
    assert_eq!(read, size);
    let _ = hash;

    println!(
        "| disk write | {size_mb} MB | {:.0} MiB/s | {:.0} Mbps |",
        mibs(size as u64, w),
        mbps(size as u64, w)
    );
    println!(
        "| disk read | {size_mb} MB | {:.0} MiB/s | {:.0} Mbps |",
        mibs(size as u64, r),
        mbps(size as u64, r)
    );
    println!(
        "| blake3 hash | {size_mb} MB | {:.0} MiB/s | {:.0} Mbps |",
        mibs(size as u64, h),
        mbps(size as u64, h)
    );
    std::fs::remove_file(&path).ok();
    Ok(())
}

fn cfg_for(base: &Path, name: &str, port: u16, streams: Option<u32>) -> NodeConfig {
    let mut cfg = NodeConfig::new(
        base.join(format!("{name}-state")),
        name.into(),
        DeviceKind::Other,
    );
    cfg.receive_dir = base.join(format!("{name}-recv"));
    cfg.relay = dropbridge_network::RelayConfig::Disabled;
    cfg.announce = false;
    cfg.auto_receive = true;
    cfg.pairing_auto_approve = true;
    cfg.fixed_port = Some(port);
    cfg.override_stream_count = streams;
    cfg
}

async fn transfer_bench(base: &Path, size_mb: usize, streams: Option<u32>) -> Result<(f64, f64)> {
    let cfg_a = cfg_for(
        base,
        "bench-recv",
        46101 + streams.unwrap_or(4) as u16,
        streams,
    );
    let cfg_b = cfg_for(
        base,
        "bench-send",
        46201 + streams.unwrap_or(4) as u16,
        streams,
    );
    let node_a = Node::start(cfg_a.clone()).await?;
    let node_b = Node::start(cfg_b).await?;

    // Pair (fast path: QR string roundtrip like production).
    let inv = node_a.create_pair_invitation().await?;
    let scanned = pairing::parse_qr(&pairing::invitation_qr_string(&inv)?)?;
    node_b.join_pairing(scanned).await?;

    // Wait until trust converges on both sides (same as lan_e2e).
    for _ in 0..100 {
        let ab = node_a.trusted_devices().await;
        let ba = node_b.trusted_devices().await;
        if !ab.is_empty() && !ba.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let file = base.join(format!("payload-{size_mb}mb.bin"));
    write_test_file(&file, size_mb * 1024 * 1024)?;
    let size = (size_mb * 1024 * 1024) as u64;

    let t = Instant::now();
    let res = session::send_files(
        &node_b,
        &node_a.device_id(),
        TransferSource::Paths(vec![file.clone()]),
    )
    .await?;
    let elapsed = t.elapsed();
    assert!(res.ok, "transfer must verify");

    node_a.endpoint().close().await;
    node_b.endpoint().close().await;
    std::fs::remove_file(&file).ok();
    let _ = std::fs::remove_dir_all(&cfg_a.receive_dir);
    Ok((mibs(size, elapsed), mbps(size, elapsed)))
}

fn zstd_sample() {
    // Two samples: clearly compressible (text-ish) and media-ish (random).
    let text: Vec<u8> = b"The quick brown fox jumps over the lazy dog. "
        .iter()
        .cycle()
        .take(8 * 1024 * 1024)
        .copied()
        .collect();
    let rnd: Vec<u8> = (0..8 * 1024 * 1024u32)
        .map(|i| (i * 31 % 251) as u8)
        .collect();
    for (name, data) in [("compressible", &text), ("media-like", &rnd)] {
        let t = Instant::now();
        let out = zstd::encode_all(&data[..], 3).unwrap_or_default();
        let e = t.elapsed();
        let ratio = data.len() as f64 / out.len().max(1) as f64;
        println!(
            "| zstd-3 {name} | {} MB→{:.1} MB | ratio {:.1}x | {:.0} MiB/s |",
            data.len() / 1024 / 1024,
            out.len() as f64 / 1024.0 / 1024.0,
            ratio,
            mibs(data.len() as u64, e)
        );
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let tmp;
    let base = match &cli.dir {
        Some(d) => {
            std::fs::create_dir_all(d)?;
            d.clone()
        }
        None => {
            tmp = tempfile::tempdir()?;
            tmp.path().to_path_buf()
        }
    };

    let disk_mb = if cli.quick { 64 } else { 256 };
    let xfer_mb = if cli.quick { 16 } else { 128 };

    println!("## DropBridge benchmark");
    println!();
    println!("| metric | size | result | |");
    println!("|---|---|---|---|");

    disk_bench(&base, disk_mb).await?;

    println!("| zstd sample | — | see below | |");
    zstd_sample();

    let (mi, mb) = transfer_bench(&base, xfer_mb, None).await?;
    println!(
        "| loopback transfer (default streams) | {xfer_mb} MB | {mi:.0} MiB/s | {mb:.0} Mbps |"
    );

    if cli.matrix {
        for s in [1u32, 2, 4, 8] {
            let (mi, mb) = transfer_bench(&base, xfer_mb, Some(s)).await?;
            println!(
                "| loopback transfer ({s} streams) | {xfer_mb} MB | {mi:.0} MiB/s | {mb:.0} Mbps |"
            );
        }
    }

    println!();
    println!("_loopback = sender+receiver in one process on this machine; real LAN numbers in docs/BENCHMARKS.md_");
    Ok(())
}
