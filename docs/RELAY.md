# DropBridge Relay & Rendezvous Operations Guide

DropBridge ships **two** server components. Both are optional for LAN use and
both are *blind*: they never see plaintext files.

| Component | Purpose | Sees |
|---|---|---|
| `dropbridge-rendezvous` | presence, endpoint hints, wake (push) hints | device ids, *signed opaque* blobs, timestamps |
| `dropbridge-relay` | QUIC relay when direct P2P is impossible | opaque QUIC packets + byte counts |

Neither stores file content. Neither can decrypt anything.

---

## 1. Rendezvous (`server/rendezvous`)

### What it does
* `POST /v1/presence` — store a signed presence blob (endpoint address hints,
  relay URL, capabilities). Signed with the device's Ed25519 key; the server
  verifies the signature and stores the blob opaquely.
* `GET /v1/presence/{device}` — peers fetch hints for a trusted device.
  Presence older than 120 s is reported as offline.
* `POST /v1/wake` / `GET /v1/wake/{device}` — sender deposits a short-lived
  wake ticket for an offline device (drives Android push / Windows wake
  decisions); the receiver polls when it comes online.
* `GET /healthz` — ops.

### Running it

```bash
cargo run -p dropbridge-rendezvous --release -- \
  --bind 0.0.0.0:8090 \
  --db /var/lib/dropbridge/rendezvous.sqlite   # or "memory" for ephemeral
```

Stateless-ish scaling: one SQLite file per instance is fine for self-hosted
scale; cap is 1 M devices per instance (LRU eviction).

### Hardening
* Put TLS in front (Caddy/nginx) — the binary speaks plain HTTP; see
  `deploy/relay/Caddyfile` for a combined example.
* Presence blobs are garbage-collected by TTL (120 s) on read and by the
  device cap on write.

---

## 2. Relay (`dropbridge-relay`)

A thin wrapper around **iroh-relay** (the reference relay from the iroh
project, same code that powers their public relay network). TLS is mandatory
in production (iroh clients refuse plaintext relays).

### Quick start (Docker)

```bash
cd deploy/relay
docker compose up -d
```

What comes up:
* `dropbridge-relay` on 127.0.0.1:8080 (plain HTTP relay endpoint),
* Caddy in front on 443 with automatic Let's Encrypt certs (set `DOMAIN` in
  the compose file) — iroh clients require TLS, so never expose 8080 directly,
* Prometheus metrics on 127.0.0.1:9090.

### Manual run

```bash
cargo run -p dropbridge-relay --release -- \
  --bind 127.0.0.1:8080 \
  --metrics 127.0.0.1:9090 \
  --rate-limit-mbps 100        # 0 = unlimited
```

### Config knobs (mapped to iroh-relay `ServerConfig`)
* `--rate-limit-mbps` → `ClientRateLimit` — per-client bandwidth cap
  (prevents one device from starving others; spec §60).
* `Limits` — request/response buffer caps (defaults in the wrapper).
* `AllowAll` ACL by default; put auth in front (Caddy basic-auth / IP allow
  list) if you want a closed relay.

### Pointing clients at your relay

```bash
dropbridge daemon --relay urls=https://relay.example.com
# or per-invocation:
dropbridge send --relay urls=https://relay.example.com file.bin
```

`--relay disabled` forces LAN/internet-direct only; `--relay n0` uses the
default public relay network (not recommended for privacy-sensitive use —
run your own).

### Observability
* Prometheus metrics: connections, bytes relayed per client, dial errors.
* Logs: structured tracing (`RUST_LOG=dropbridge_relay=info`).
* The relay is *replaceable mid-session*: iroh endpoints migrate to a better
  path; relays can drain by refusing new connections.

---

## 3. Deployment topology

```
            ┌────────────┐        signed blobs only
 Android ──►│ rendezvous │◄── Windows
            └────────────┘
                │ wake tickets
                ▼
        ┌──────────────┐   opaque QUIC   ┌──────────────┐
        │ dropbridge-  │◄───────────────►│ dropbridge-  │
        │ relay (443)  │  when no direct │ relay backup │
        └──────────────┘     path        └──────────────┘
```

* One relay handles thousands of concurrent sessions (it only forwards
  packets); scale horizontally behind DNS.
* Keep relay and rendezvous on separate hosts if you want failure isolation.
* Costs: bandwidth only. A 1 GB transfer costs the relay ~1 GB in + 1 GB out
  *only* when no direct path existed.

## 4. Self-host checklist
- [ ] Valid TLS cert for the relay domain (ACME or manual)
- [ ] UDP **and** TCP 443 open (QUIC prefers UDP; falls back to TCP/443)
- [ ] Metrics scraped + alert on `relay_dial_errors` rate
- [ ] Rate limits set for your pipe size
- [ ] Backups: rendezvous SQLite is disposable (hints re-announce); no backup needed
