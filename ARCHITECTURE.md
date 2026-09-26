# DropBridge Architecture

> "Pair once. Drop anywhere. Delivered automatically."

```
┌─────────────── Android app (Flutter) ───────────────┐   ┌────────── Windows (tray + Flutter UI) ─────────┐
│  Share sheet target · home screen · transfer cards  │   │  dropbridge-tray (menu, autostart, folders)     │
│  Permission UX (NEARBY_WIFI_DEVICES, etc.)          │   │  dropbridge daemon (engine)                     │
└───────────────┬─────────────────────────────────────┘   └───────────────┬─────────────────────────────────┘
                │ FFI: commands / metadata / events only — never file bytes (spec §26)
┌───────────────▼────────────────────────────────────────────────────────▼─────────────────────────────────┐
│                                   dropbridge-core (Rust)                                                   │
│  Node orchestration · pairing/trust · session state machines · events                                      │
│  ┌──────────────┐ ┌───────────────┐ ┌───────────────┐ ┌──────────────┐ ┌──────────────┐                    │
│  │ identity     │ │ discovery     │ │ transfer      │ │ network      │ │ protocol     │                    │
│  │ Ed25519 keys │ │ mDNS + signed │ │ chunk engine  │ │ iroh adapter │ │ v1 messages  │                    │
│  │ DPAPI/Keystore│ │ UDP beacon   │ │ SQLite journal│ │ path manager │ │ path safety  │                    │
│  │ trust store  │ │ bounded scans │ │ BLAKE3, resume│ │ scoring      │ │ limits       │                    │
│  └──────────────┘ └───────────────┘ └───────────────┘ └──────┬───────┘ └──────────────┘                    │
└──────────────────────────────────────────────────────────────┼─────────────────────────────────────────────┘
                                            iroh 1.2 (QUIC, dial-by-key, NAT traversal, migration)
                        ┌────────────────────────────┬───────────────────────┐
                        ▼                            ▼                       ▼
                 LAN direct (QUIC)        Internet direct (hole-punched)   Encrypted relay (blind)
                                                                             iroh-relay (self-hosted)
                                                            rendezvous (signed presence + wake hints only)
```

## Principles

1. **Core is platform-neutral Rust.** Android and Windows are thin shells.
   One security-critical code path to audit; macOS/Linux/iOS become shells too.
2. **Bytes never cross the FFI bridge.** Dart/Kotlin/WinUI see commands,
   handles, metadata and progress events; gigabytes move disk → Rust buffer →
   QUIC inside Rust (spec §26, §68).
3. **Identity-keyed networking.** Devices dial *keys*, not IPs (iroh). Routing
   (LAN / P2P / relay) is automatic and invisible to users (spec §84).
4. **Offline-first.** LAN transfers never touch the cloud; rendezvous/relay
   exist only for reachability (spec §7).
5. **Resilience by journal.** Every completed range is persisted; crashes,
   restarts and network changes resume near the breakpoint (spec §39–40).
6. **Blind infrastructure.** Relays forward opaque packets; rendezvous stores
   signed opaque blobs. Nobody upstream can read files (spec §8, §22).

## Crate map

| Crate | Role |
|---|---|
| `dropbridge-protocol` | Protocol v1: messages, framing, manifests, limits, path safety |
| `dropbridge-identity` | Ed25519 identity, secret protectors (DPAPI/file/Keystore hook), trust registry |
| `dropbridge-discovery` | mDNS/DNS-SD + signed UDP beacon, bounded browsing |
| `dropbridge-transfer` | Chunk engine: streaming, BLAKE3, SQLite journal, resume, atomic receive |
| `dropbridge-network` | iroh endpoint builder, QUIC stream adapters, path scoring/hysteresis |
| `dropbridge-core` | Node: pairing, sessions, events — the product brain |
| `dropbridge-cli` | Reference app: pair/daemon/send/discover (also the Windows daemon) |
| `dropbridge-tray` | Windows tray shell (menu, autostart, daemon supervision) |
| `dropbridge-ffi` | C-ABI surface for Flutter (JSON commands/events) |
| `dropbridge-relay` | Self-hosted blind relay (iroh-relay wrapper) |
| `dropbridge-rendezvous` | Minimal control plane (Axum): presence + wake hints |
| `dropbridge-bench` | Benchmark suite (disk/hash/loopback/streams/zstd) |

## Key flows

### Pairing (once)
Windows shows QR (`dropbridge pair`) → phone scans → QUIC dial to enroller
key → one-time token + 6-digit auth code + human confirm → both sides store
each other as Trusted with permissions.

### Android → Windows (same LAN)
Share → DropBridge → core builds manifest → discovery already knows the
laptop's LAN address → QUIC direct → offer/accept (resume ranges) → N chunk
streams → BLAKE3 verify → atomic rename into `DropBridge\From Phone` →
notification.

### Phone on 5G, laptop behind home NAT
Same UX. iroh attempts direct P2P (hole punch); if impossible, the session
flows over the encrypted relay transparently. If the connection dies mid-way,
the journal resumes.

### Windows → Android
Drop file into `DropBridge\To Phone` (stability-checked, spec §34) → queued →
sent to trusted phone. Wake path for a stopped Android app: rendezvous wake
hint → OS push channel → app starts FGS → E2E transfer (spec §57–58).

## Testing strategy

* protocol: roundtrip + proptest (paths, ranges, framing)
* transfer: unit tests incl. idempotent ranges, collision policies
* core: LAN E2E in CI — pair → directory transfer → resume-after-fault →
  untrusted refusal (`tests/lan_e2e.rs`)
* chaos hooks: deterministic fault injection (`test_recv_fail_after`)
* benchmarks in CI + real-LAN numbers in docs/BENCHMARKS.md

Verification status of platform-specific paths (Android runtime permissions,
Windows tray/DPAPI on real machines, Wi-Fi Direct) is tracked honestly in
docs/TESTING_STATUS.md.
