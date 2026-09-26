# DropBridge

**Pair once. Drop anywhere. Delivered automatically.**

DropBridge moves files between your Android phone and your Windows PC —
no accounts, no cloud storage, no cables. Share a photo from any app, or
drop a folder into `DropBridge\To Phone`; it arrives on the other device
over the best available route, encrypted end-to-end, and resumable if the
network dies mid-way.

```
Android Share sheet ──► DropBridge ──► your PC's "From Phone" folder
PC "To Phone" folder ──► DropBridge ──► your phone
```

## How it moves bytes

The app scores available paths and migrates between them invisibly:

| Path | When | Properties |
|---|---|---|
| **1. LAN direct** | same Wi-Fi (default) | fastest; works fully offline |
| **2. Internet P2P** | different networks | QUIC hole-punch (~90% success via iroh) |
| **3. Encrypted relay** | P2P impossible | self-hostable; blind (can't decrypt) |

Built on **iroh 1.2** (QUIC, NAT traversal, connection migration, relay) —
see `docs/ADR-001-network-stack.md` for the comparison and
`docs/TECHNOLOGY_REVIEW_2026.md` for the 2026 stack survey.

## Feature highlights

* **Resilient transfers**: persistent SQLite journal + ranged resume —
  survive Wi-Fi drops, sleep, even restarts. BLAKE3 verification of every
  byte, atomic landing (`*.dropbridge-part` → rename).
* **Streaming engine**: constant memory on 100 GB transfers; parallel chunk
  streams (benchmark-tuned); small files batched; directories preserved.
* **Accountless QR pairing** with human-confirmed 6-digit code; identities
  are Ed25519 keys in Android Keystore / Windows DPAPI.
* **Offline-first**: LAN mode needs zero servers; rendezvous/relay are
  optional, self-hostable, and blind.
* One Rust core for all platforms; Flutter UIs are thin shells; file bytes
  never cross the language bridge.

## Repository layout

```
dropbridge/
├── rust/crates/         core, protocol, transfer, network, identity,
│                        discovery, cli, tray, ffi, relay, bench
├── server/rendezvous/   minimal control plane (Axum)
├── app/flutter/         Android + Windows UI shell
├── deploy/relay/        Docker/Caddy for the self-hosted relay
├── integration-tests/   cross-device test plans + scripts
├── benchmarks/          recorded results + methodology
└── docs/                architecture, protocol, security, ADRs, guides
```

## Quick start (today: CLI reference app)

```bash
cd dropbridge
cargo build --release -p dropbridge-cli

# on the PC:
./target/release/dropbridge daemon --port 44013    # receive + outbox watcher
./target/release/dropbridge pair                     # show QR

# on the phone (or second machine):
./target/release/dropbridge join "<qr string>"       # scan/type the QR text

# send:
./target/release/dropbridge send laptop ./photos
```

Flutter apps (`app/flutter`) use the same engine through the C-ABI FFI
(`rust/crates/dropbridge-ffi`).

## Self-hosting servers

* Relay: `deploy/relay` (docker compose + Caddy TLS) — docs/RELAY.md
* Rendezvous: `cargo run -p dropbridge-rendezvous -- --bind 0.0.0.0:8090`

## Documentation map

| Doc | Contents |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | system design, crate map, flows |
| [docs/PROTOCOL.md](docs/PROTOCOL.md) | wire protocol v1 (versioned) |
| [docs/SECURITY.md](SECURITY.md) + [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md) | security model & adversaries |
| [docs/TECHNOLOGY_REVIEW_2026.md](docs/TECHNOLOGY_REVIEW_2026.md) | 2026 stack research |
| [docs/ADR-001-network-stack.md](docs/ADR-001-network-stack.md) | Iroh vs Quinn vs libp2p vs WebRTC |
| [docs/ANDROID.md](docs/ANDROID.md) / [docs/WINDOWS.md](docs/WINDOWS.md) | platform deep-dives |
| [docs/RELAY.md](docs/RELAY.md) | relay/rendezvous operations |
| [docs/BENCHMARKS.md](docs/BENCHMARKS.md) | numbers + methodology |
| [docs/TESTING_STATUS.md](docs/TESTING_STATUS.md) | what is verified, honestly |
| [CONTRIBUTING.md](CONTRIBUTING.md) | dev workflow |

## Status

Phase 0–4 of the roadmap are implemented in Rust (research → LAN MVP →
bidirectional engine → internet P2P → relay + rendezvous). Android APK and
Windows installer packaging are next; see docs/TESTING_STATUS.md for the
verified/unverified ledger.

## License

MIT OR Apache-2.0 (matching the iroh ecosystem we build on).
