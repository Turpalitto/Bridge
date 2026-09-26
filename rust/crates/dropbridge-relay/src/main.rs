//! DropBridge relay: a *blind encrypted packet forwarder* (spec §21–22).
//!
//! Thin production wrapper around `iroh_relay::server`:
//! * the relay only sees opaque QUIC packets addressed by EndpointId —
//!   it has no keys and cannot decrypt user files,
//! * per-client rate limiting,
//! * Prometheus metrics endpoint (feature `metrics`),
//! * structured logging; no file contents, no filenames, ever.
//!
//! TLS: run behind your TLS-terminating proxy (Caddy/nginx) or use the
//! official iroh-relay binary for Let's Encrypt automation; see
//! dropbridge/deploy/relay/README.md.
use std::net::SocketAddr;
use std::num::NonZeroU32;

use anyhow::Result;
use clap::Parser;
use iroh_relay::server::{AllowAll, ClientRateLimit, Limits, RelayConfig, Server, ServerConfig};
use tracing::info;

#[derive(Parser)]
#[command(
    name = "dropbridge-relay",
    version,
    about = "DropBridge self-hosted blind relay"
)]
struct Cli {
    /// HTTP bind address for the relay service.
    #[arg(long, default_value = "0.0.0.0:8080")]
    bind: SocketAddr,

    /// Prometheus metrics bind address (0/disabled with --no-metrics).
    #[arg(long, default_value = "0.0.0.0:9090")]
    metrics: SocketAddr,

    #[arg(long)]
    no_metrics: bool,

    /// Per-client receive rate limit in Mbit/s (0 = unlimited).
    #[arg(long, default_value_t = 0)]
    rate_limit_mbps: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();

    let mut relay = RelayConfig::new(cli.bind);
    relay.access = std::sync::Arc::new(AllowAll);
    if cli.rate_limit_mbps > 0 {
        let bytes_per_second =
            NonZeroU32::new(cli.rate_limit_mbps.saturating_mul(125_000)).unwrap();
        // `Limits` is #[non_exhaustive] — build via Default + field assignment.
        let mut limits = Limits::default();
        limits.client_rx = Some(ClientRateLimit::new(bytes_per_second));
        relay.limits = limits;
    }

    // `ServerConfig` is #[non_exhaustive] — same construction style.
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    if !cli.no_metrics {
        config.metrics_addr = Some(cli.metrics);
    }

    let server = Server::spawn(config)
        .await
        .map_err(|e| anyhow::anyhow!("relay spawn: {e}"))?;
    info!(
        http = %cli.bind,
        metrics = if cli.no_metrics { "off".to_string() } else { cli.metrics.to_string() },
        rate_limit_mbps = cli.rate_limit_mbps,
        "dropbridge relay running (blind forwarder — cannot read your files)"
    );

    // Run until interrupted.
    tokio::signal::ctrl_c().await?;
    info!("shutting down");
    drop(server);
    Ok(())
}
