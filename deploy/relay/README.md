# Self-hosted DropBridge relay

The relay is a **blind forwarder**: it relays opaque QUIC packets between
endpoints. It has no keys, sees no plaintext, no filenames, no contents —
only connection metadata and byte counts.

## Quick start

```bash
docker compose up -d --build
```

Edit `Caddyfile`: replace `relay.example.com` with your domain. Caddy
obtains Let's Encrypt certificates automatically. iroh clients require
`https://` relay URLs, so never expose the relay HTTP port directly.

Point clients at it:

```bash
dropbridge daemon --relay urls=https://relay.example.com
```

## Endpoints

| Where | What |
|---|---|
| `https://relay.example.com/` | relay service (via Caddy → `relay:8080`) |
| `127.0.0.1:9090/metrics` | Prometheus metrics (keep private) |

## Ops knobs

* Rate limit: set `--rate-limit-mbps` in the compose `command` (default
  1000 Mbit/s per client).
* Logs: `docker compose logs relay` (structured tracing; tune `RUST_LOG`).
* Upgrade: `docker compose pull && docker compose up -d` — active sessions
  drop and clients reconnect/migrate automatically.

See [docs/RELAY.md](../../docs/RELAY.md) for the full operations guide.
