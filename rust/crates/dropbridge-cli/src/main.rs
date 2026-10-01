//! DropBridge reference CLI.
//!
//! ```text
//! dropbridge info                       show this device
//! dropbridge pair [--auto-confirm]      show pairing QR (enroller)
//! dropbridge join <qr-string>           scan/paste a pairing QR (joiner)
//! dropbridge devices                    list trusted devices
//! dropbridge discover [--secs N]        browse the LAN
//! dropbridge daemon                     run receiver/announcer in foreground
//! dropbridge send <peer> <path>...      send files to a trusted device
//! ```
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use dropbridge_core::config::NodeConfig;
use dropbridge_core::events::NodeEvent;
use dropbridge_core::node::Node;
use dropbridge_core::pairing;
use dropbridge_core::session;
use dropbridge_protocol::DeviceKind;

mod shell;
mod shutdown;

#[derive(Parser)]
#[command(
    name = "dropbridge",
    version,
    about = "DropBridge — pair once, drop anywhere",
    long_about = None
)]
struct Cli {
    /// State directory (keys, trust, journal).
    #[arg(long, global = true)]
    state: Option<PathBuf>,
    /// Receive directory.
    #[arg(long, global = true)]
    receive: Option<PathBuf>,
    /// Device name.
    #[arg(long, global = true)]
    name: Option<String>,
    /// Device kind: phone|tablet|laptop|desktop.
    #[arg(long, global = true, default_value = "laptop")]
    kind: String,
    /// Relay mode: n0 | disabled | url1,url2
    #[arg(long, global = true, default_value = "n0")]
    relay: String,
    /// Bind a fixed UDP port (stable hints + single firewall rule).
    #[arg(long, global = true)]
    port: Option<u16>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Info,
    Pair {
        #[arg(long)]
        auto_confirm: bool,
    },
    Join {
        qr: String,
    },
    Devices,
    Discover {
        #[arg(long, default_value_t = 6)]
        secs: u64,
    },
    Daemon,
    Send {
        peer: String,
        #[arg(num_args = 0..)]
        paths: Vec<PathBuf>,
        /// Resume a previous session id instead of starting a new one.
        #[arg(long)]
        session: Option<u64>,
    },
    /// Manage Windows Explorer context menu integration
    Shell {
        #[command(subcommand)]
        action: ShellSubcommand,
    },
    /// Manage continuously synchronized folders
    Sync {
        #[command(subcommand)]
        action: SyncSubcommand,
    },
}

#[derive(Subcommand)]
enum SyncSubcommand {
    /// Add a directory for continuous automatic synchronization
    Add {
        /// Local directory path to synchronize
        path: PathBuf,
        /// Target device name, id prefix, or 'auto' (default: auto)
        #[arg(long, default_value = "auto")]
        target: String,
    },
    /// Remove a directory from synchronization
    Remove {
        /// Local directory path to remove
        path: PathBuf,
    },
    /// List all configured sync folders
    List,
}

#[derive(Subcommand)]
enum ShellSubcommand {
    /// Register "Send via DropBridge" context menu in Windows Explorer
    Install,
    /// Remove "Send via DropBridge" from Windows Explorer
    Uninstall,
    /// Check whether Explorer shell integration is active
    Status,
}

fn default_state_dir() -> PathBuf {
    if let Ok(v) = std::env::var("DROPBRIDGE_HOME") {
        return PathBuf::from(v);
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".dropbridge")
}

fn parse_kind(s: &str) -> DeviceKind {
    match s {
        "phone" => DeviceKind::Phone,
        "tablet" => DeviceKind::Tablet,
        "desktop" => DeviceKind::Desktop,
        _ => DeviceKind::Laptop,
    }
}

fn parse_relay(s: &str) -> dropbridge_network::RelayConfig {
    match s {
        "disabled" | "lan" => dropbridge_network::RelayConfig::Disabled,
        "n0" | "default" => dropbridge_network::RelayConfig::N0Default,
        urls => dropbridge_network::RelayConfig::Custom(
            urls.split(',')
                .map(|u| u.trim().to_string())
                .filter(|u| !u.is_empty())
                .collect(),
        ),
    }
}

fn make_config(cli: &Cli) -> NodeConfig {
    let base = cli.state.clone().unwrap_or_else(default_state_dir);
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "device".into());
    let name = cli
        .name
        .clone()
        .unwrap_or_else(|| format!("{} (CLI)", hostname));
    let mut cfg = NodeConfig::new(base, name, parse_kind(&cli.kind));
    if let Some(r) = &cli.receive {
        cfg.receive_dir = r.clone();
    } else {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| ".".into());
        cfg.receive_dir = PathBuf::from(home).join("DropBridge").join("From Phone");
    }
    cfg.relay = parse_relay(&cli.relay);
    cfg.fixed_port = cli.port;
    cfg
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let cli = Cli::parse();
    // Installed before any long-running work so a console close (or the
    // tray's GenerateConsoleCtrlEvent) is honoured even during startup.
    shutdown::install();

    match &cli.cmd {
        Cmd::Info => {
            let cfg = make_config(&cli);
            let node = Node::start(cfg).await?;
            println!("device id : {}", node.device_id_z32());
            println!("name      : {}", node.config().device_name);
            println!("receive   : {}", node.config().receive_dir.display());
            println!("state     : {}", node.config().state_dir.display());
            let hints = dropbridge_network::AddrHints::from_endpoint(node.endpoint());
            println!("relays    : {:?}", hints.relay_urls);
            println!("direct    : {:?}", hints.direct);
            node.close().await;
        }
        Cmd::Pair { auto_confirm } => {
            let mut cfg = make_config(&cli);
            cfg.pairing_auto_approve = *auto_confirm;
            let node = Node::start(cfg).await?;
            let inv = node.create_pair_invitation().await?;
            let qr = pairing::invitation_qr_string(&inv)?;
            println!("Scan with DropBridge on your other device (expires in 120 s):");
            println!();
            print_qr(&qr)?;
            println!();
            println!("Or paste this string: {qr}");
            if !auto_confirm {
                println!(
                    "Waiting for a phone to connect… approve with `dropbridge approve pairing yes`"
                );
            }
            // Watch events for pairing completion.
            let mut events = node.events();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(130);
            let pair_res = async {
                loop {
                    let timeout = tokio::time::sleep_until(deadline);
                    tokio::pin!(timeout);
                    tokio::select! {
                        _ = &mut timeout => { bail!("pairing window expired"); }
                        ev = events.recv() => {
                            match ev {
                                Ok(NodeEvent::PairingChallenge { device_name, auth_code, .. }) => {
                                    println!("→ {device_name} wants to pair. Confirm code: {auth_code:06}");
                                }
                                Ok(NodeEvent::TrustChanged { trusted: true, name, .. }) => {
                                    println!("✓ Paired with {name}");
                                    return Ok(());
                                }
                                Ok(_) => {}
                                Err(_) => break,
                            }
                        }
                    }
                }
                bail!("event loop ended without pairing")
            }
            .await;
            node.close().await;
            pair_res?;
        }
        Cmd::Join { qr } => {
            let cfg = make_config(&cli);
            let node = Node::start(cfg).await?;
            let qr = if qr == "-" {
                let mut s = String::new();
                std::io::stdin().read_line(&mut s)?;
                s
            } else {
                qr.clone()
            };
            let inv = pairing::parse_qr(qr.trim())?;
            println!("Pairing with {}…", inv.device_name);
            let join_res = node.join_pairing(inv).await;
            node.close().await;
            join_res?;
            println!("✓ Paired. This device is now trusted.");
        }
        Cmd::Devices => {
            let cfg = make_config(&cli);
            let node = Node::start(cfg).await?;
            let devices = node.trusted_devices().await;
            if devices.is_empty() {
                println!("No trusted devices. Run `dropbridge pair` to add one.");
            }
            for d in devices {
                let id: dropbridge_identity::DeviceId = d.device_id;
                let z = dropbridge_network::hints::id_z32(&id);
                println!("{:<24} {}  perms={:#08x}", d.name, z, d.permissions);
            }
            node.close().await;
        }
        Cmd::Discover { secs } => {
            let cfg = make_config(&cli);
            let node = Node::start(cfg).await?;
            println!("Browsing for DropBridge devices ({} s)…", secs);
            let found = node.discover(Duration::from_secs(*secs)).await;
            if found.is_empty() {
                println!("Nothing found. Is the other device running `dropbridge daemon`?");
            }
            for p in found {
                println!(
                    "{:<24} {}  via {:?} at {}",
                    p.name,
                    dropbridge_network::hints::id_z32(&p.device_id),
                    p.source,
                    p.addr
                );
            }
            node.close().await;
        }

        Cmd::Daemon => {
            let cfg = make_config(&cli);
            let node = Node::start(cfg).await?;
            println!(
                "DropBridge daemon: {} ({})",
                node.config().device_name,
                node.device_id_z32()
            );
            println!("Receiving into: {}", node.config().receive_dir.display());
            // Outbox: drop files into "To Phone" next to the receive dir.
            let outbox = node
                .config()
                .receive_dir
                .parent()
                .map(|p| p.join("To Phone"))
                .unwrap_or_else(|| PathBuf::from("To Phone"));
            let _watcher = dropbridge_core::watcher::spawn_outbox_watcher(
                node.clone(),
                outbox.clone(),
                dropbridge_core::watcher::OutboxTarget::Auto,
            )?;
            println!("Watching outbox: {}", outbox.display());
            let sync_folders = node.list_sync_folders();
            if !sync_folders.is_empty() {
                println!("Active sync folders ({}):", sync_folders.len());
                for f in &sync_folders {
                    println!("  • {}", f.path.display());
                }
            }
            let mut events = node.events();
            let mut last_progress_print = std::time::Instant::now() - Duration::from_secs(2);
            let mut acc_bytes = 0u64;
            // Graceful stop: SIGINT/SIGTERM on Unix; console control events on
            // Windows (console close, or GenerateConsoleCtrlEvent from the
            // tray). One future for the whole loop — the Windows implementation
            // polls the flag that the OS-owned handler thread flips.
            let mut shutdown = shutdown::wait();
            loop {
                let ev = tokio::select! {
                    ev = events.recv() => ev,
                    kind = &mut shutdown => {
                        println!("\n{}", kind.message());
                        break;
                    }
                };
                match ev {
                    Ok(NodeEvent::DeviceDiscovered(p)) => {
                        println!("• discovered {} at {}", p.name, p.addr);
                    }
                    Ok(NodeEvent::IncomingOffer {
                        peer_name,
                        session,
                        files,
                        total_bytes,
                        ..
                    }) => {
                        println!("⇩ offer from {peer_name}: {files} file(s), {total_bytes} bytes (session {session})");
                    }
                    Ok(NodeEvent::TransferProgress {
                        bytes_delta,
                        total_bytes,
                        ..
                    }) => {
                        acc_bytes += bytes_delta;
                        if last_progress_print.elapsed() > Duration::from_secs(1) {
                            println!("  … {} / {} bytes", human(acc_bytes), human(total_bytes));
                            last_progress_print = std::time::Instant::now();
                        }
                    }
                    Ok(NodeEvent::TransferCompleted {
                        ok, files, detail, ..
                    }) => {
                        if ok {
                            println!("✓ received {} file(s): {}", files.len(), detail);
                            for f in &files {
                                println!("   {}", f.display());
                            }
                        } else {
                            println!("✗ transfer failed: {detail}");
                        }
                        acc_bytes = 0;
                    }
                    Ok(NodeEvent::PairingChallenge {
                        device_name,
                        auth_code,
                        ..
                    }) => {
                        println!("⇄ {device_name} wants to pair — code {auth_code:06}");
                    }
                    Ok(NodeEvent::TransferFailed { reason, .. }) => {
                        println!("✗ send failed: {reason} (will retry)");
                    }
                    Ok(NodeEvent::TrustChanged { name, trusted, .. }) => {
                        println!(
                            "{} {name}",
                            if trusted {
                                "＋ trusted"
                            } else {
                                "− revoked"
                            }
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        println!("(missed {n} events)");
                    }
                    Err(_) => break,
                }
            }
            println!("Stopping daemon…");
            node.close().await;
            println!("✓ daemon stopped cleanly.");
        }
        Cmd::Send {
            peer,
            paths,
            session: resume_session,
        } => {
            let paths = if paths.is_empty() {
                #[cfg(windows)]
                {
                    pick_files_windows()?
                }
                #[cfg(not(windows))]
                {
                    bail!("no paths provided; usage: dropbridge send <peer> <path>...");
                }
            } else {
                paths.clone()
            };
            if paths.is_empty() {
                println!("No files selected.");
                return Ok(());
            }

            let cfg = make_config(&cli);
            let node = {
                let mut c = cfg.clone();
                c.announce = false;
                Node::start(c).await?
            };
            // Resolve peer by name prefix or id prefix, or auto-pick phone.
            let devices = node.trusted_devices().await;
            if devices.is_empty() {
                bail!("no trusted devices paired; run `dropbridge pair` first");
            }
            let target = if peer.eq_ignore_ascii_case("auto") {
                devices
                    .iter()
                    .find(|d| d.kind.eq_ignore_ascii_case("phone"))
                    .unwrap_or(&devices[0])
            } else {
                let matches: Vec<_> = devices
                    .iter()
                    .filter(|d| {
                        d.name.to_lowercase().contains(&peer.to_lowercase())
                            || id_prefix(&d.device_id).starts_with(&peer.to_lowercase())
                    })
                    .collect();
                if matches.is_empty() {
                    bail!("no trusted device matches {peer:?}; run `dropbridge devices`");
                }
                if matches.len() > 1 {
                    bail!("multiple devices match {peer:?}; be more specific");
                }
                matches[0]
            };
            let peer_id = target.device_id;
            println!("Sending {} path(s) to {}…", paths.len(), target.name);

            // Make sure we have a route: try discovery first (LAN-first).
            if node.hints_for(&peer_id).await.is_none()
                || cli.relay == "disabled"
                || cli.relay == "lan"
            {
                let _ = node.discover(Duration::from_secs(2)).await;
            }

            let mut events = node.events();
            let node2 = node.clone();
            let printer = tokio::spawn(async move {
                let mut last = std::time::Instant::now() - Duration::from_secs(2);
                let mut acc = 0u64;
                while let Ok(ev) = events.recv().await {
                    if let NodeEvent::TransferProgress {
                        bytes_delta,
                        total_bytes,
                        ..
                    } = ev
                    {
                        acc += bytes_delta;
                        if last.elapsed() > Duration::from_secs(1) {
                            println!("  … {} / {}", human(acc), human(total_bytes));
                            last = std::time::Instant::now();
                        }
                    }
                }
            });

            let result = match resume_session {
                Some(sid) => {
                    session::send_files_with_session(
                        &node2,
                        &peer_id,
                        dropbridge_transfer::TransferSource::Paths(paths.clone()),
                        Some(*sid),
                    )
                    .await
                }
                None => {
                    session::send_files(
                        &node2,
                        &peer_id,
                        dropbridge_transfer::TransferSource::Paths(paths.clone()),
                    )
                    .await
                }
            };
            printer.abort();
            node2.close().await;
            match result {
                Ok(r) if r.ok => println!("✓ delivered ({} bytes)", r.bytes),
                Ok(r) => bail!("peer reported failure: {}", r.detail),
                Err(e) => bail!("{e}"),
            }
        }
        Cmd::Shell { action } => match action {
            ShellSubcommand::Install => {
                shell::handle_shell_integration(shell::ShellAction::Install)?
            }
            ShellSubcommand::Uninstall => {
                shell::handle_shell_integration(shell::ShellAction::Uninstall)?
            }
            ShellSubcommand::Status => shell::handle_shell_integration(shell::ShellAction::Status)?,
        },
        Cmd::Sync { action } => {
            let cfg = make_config(&cli);
            let registry = dropbridge_core::sync::SyncRegistry::new(&cfg.state_dir);
            match action {
                SyncSubcommand::Add { path, target } => {
                    let abs_path = if path.is_absolute() {
                        path.clone()
                    } else {
                        std::env::current_dir()?.join(path)
                    };
                    std::fs::create_dir_all(&abs_path)?;
                    let tgt = if target.eq_ignore_ascii_case("auto") {
                        dropbridge_core::watcher::OutboxTarget::Auto
                    } else {
                        dropbridge_core::watcher::OutboxTarget::Device(target.clone())
                    };
                    let sf = dropbridge_core::sync::SyncFolder::new(abs_path.clone(), tgt);
                    registry.add(sf)?;
                    println!("✓ Added sync folder: {}", abs_path.display());
                    println!("  Target: {target}");
                    println!("  Changes in this folder will be automatically synchronized.");
                }
                SyncSubcommand::Remove { path } => {
                    let abs_path = if path.is_absolute() {
                        path.clone()
                    } else {
                        std::env::current_dir()?.join(path)
                    };
                    if registry.remove(&abs_path)? {
                        println!("✓ Removed sync folder: {}", abs_path.display());
                    } else {
                        println!("Folder was not found in sync list: {}", abs_path.display());
                    }
                }
                SyncSubcommand::List => {
                    let list = registry.load();
                    if list.is_empty() {
                        println!("No sync folders configured.");
                        println!("Run `dropbridge sync add <path>` to add one.");
                    } else {
                        println!("Configured sync folders ({}):", list.len());
                        for f in list {
                            let tgt_str = match &f.target {
                                dropbridge_core::watcher::OutboxTarget::Auto => "auto",
                                dropbridge_core::watcher::OutboxTarget::Device(d) => d.as_str(),
                            };
                            println!(
                                "  • {} (target: {}, enabled: {})",
                                f.path.display(),
                                tgt_str,
                                f.enabled
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn id_prefix(id: &dropbridge_identity::DeviceId) -> String {
    dropbridge_network::hints::id_z32(id)
        .chars()
        .take(8)
        .collect::<String>()
        .to_lowercase()
}

fn human(n: u64) -> String {
    const U: [f64; 5] = [
        1.0,
        1024.0,
        1024.0 * 1024.0,
        1024.0 * 1024.0 * 1024.0,
        1024.0 * 1024.0 * 1024.0 * 1024.0,
    ];
    let i = (63 - n.leading_zeros()).min(40) / 10;
    let i = i.min(4) as usize;
    format!(
        "{:.1} {}",
        n as f64 / U[i],
        ["B", "KiB", "MiB", "GiB", "TiB"][i]
    )
}

/// Render a pairing QR as pure ASCII.
///
/// The tray opens `dropbridge pair` in a brand-new console, which on a stock
/// Russian Windows uses an OEM code page (866/1251) — the Unicode half-block
/// renderer used to print mojibake there. Two `#` per dark module keeps the
/// aspect ratio of a character cell, so the code still scans.
fn print_qr(text: &str) -> Result<()> {
    println!("{}", qr_ascii(text)?);
    Ok(())
}

/// Pure-ASCII QR art, `#` blocks and spaces only.
fn qr_ascii(text: &str) -> Result<String> {
    const QUIET: usize = 2;
    let code = qrcode::QrCode::new(text.as_bytes()).context("qr encode")?;
    let width = code.width();
    let dark = code.to_colors();
    let row_width = 2 * (width + 2 * QUIET);
    let blank = " ".repeat(row_width);
    let mut out = String::with_capacity((row_width + 1) * (width + 2 * QUIET));
    out.push_str(&blank);
    out.push('\n');
    for y in 0..width + 2 * QUIET {
        for x in 0..width + 2 * QUIET {
            let inside = y >= QUIET && y < QUIET + width && x >= QUIET && x < QUIET + width;
            let is_dark = inside && dark[(y - QUIET) * width + (x - QUIET)] == qrcode::Color::Dark;
            out.push_str(if is_dark { "##" } else { "  " });
        }
        out.push('\n');
    }
    out.push_str(&blank);
    Ok(out)
}

#[cfg(windows)]
fn pick_files_windows() -> anyhow::Result<Vec<PathBuf>> {
    let script = "[System.Reflection.Assembly]::LoadWithPartialName('System.windows.forms') | Out-Null; $f = New-Object System.Windows.Forms.OpenFileDialog; $f.Multiselect = $true; $f.Title = 'DropBridge — Выберите файлы для отправки'; if ($f.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { $f.FileNames }";
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let paths: Vec<PathBuf> = text
        .lines()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_is_pure_ascii_and_square_enough_to_scan() {
        let art = qr_ascii("dropbridge://pair?token=abc123").unwrap();
        assert!(
            art.is_ascii(),
            "a non-UTF8 console code page cannot print this"
        );
        let lines: Vec<&str> = art.lines().collect();
        let modules = lines[0].len() / 2;
        assert_eq!(
            lines.len(),
            modules + 2,
            "one character row per module row, plus the quiet zone"
        );
        for line in &lines {
            assert_eq!(line.len(), lines[0].len());
            assert!(line.chars().all(|c| c == '#' || c == ' '));
        }
        assert!(art.contains("##"), "the code must contain dark modules");
    }
}
