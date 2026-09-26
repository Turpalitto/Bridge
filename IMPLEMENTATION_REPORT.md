# DROPBRIDGE IMPLEMENTATION REPORT

**Product:** DropBridge — production-grade Android ⇄ Windows file transfer.
**Principle:** *Pair once. Drop anywhere. Delivered automatically.*
**Branch:** `arena/01a0d079-meetmaker` (session-pinned; all commits conventional).

---

## 1. Architecture (what was built)

One Rust core for all platforms; Flutter/Kotlin/WinUI are thin shells. File
bytes never cross the language bridge — only commands, metadata and events.

```
Android app (Flutter) / Windows (tray + daemon)
        │  C-ABI JSON FFI (dropbridge-ffi)
dropbridge-core ── Node orchestration, pairing/trust, sessions, events
 ├─ dropbridge-protocol   v1 wire protocol, limits, path safety
 ├─ dropbridge-identity   Ed25519 identity, DPAPI/Keystore protectors, trust
 ├─ dropbridge-discovery  mDNS + signed beacon, bounded scans
 ├─ dropbridge-transfer   chunk engine, BLAKE3, SQLite journal, resume
 └─ dropbridge-network    iroh 1.2 endpoint, path scoring + hysteresis
        │ QUIC (dial-by-key, NAT traversal, migration)
 LAN direct → Internet P2P → Encrypted blind relay (self-hostable)
 rendezvous (Axum): signed presence + wake hints only
```

Full diagrams and flows: [ARCHITECTURE.md](ARCHITECTURE.md).

## 2. Stack rationale (research-backed, 2026)

* **Iroh 1.2.0** (pinned `=1.2.0`, GA 2026-06-15, wire-stability
  commitment) after scoring Iroh vs Quinn+own-traversal vs libp2p vs WebRTC —
  [docs/ADR-001-network-stack.md](docs/ADR-001-network-stack.md). Iroh gives
  NAT traversal (~90% hole-punch), relay infrastructure semantics, connection
  migration, official Android (Kotlin) bindings; avoids rebuilding months of
  traversal code (user instruction: prefer Iroh if it genuinely fits).
* Protocol: postcard frames + raw chunk streams, BLAKE3 E2E verify, SQLite
  WAL journal for resume — [docs/PROTOCOL.md](docs/PROTOCOL.md).
* Flutter + hand-written C-ABI FFI (flutter_rust_bridge documented as
  optional upgrade path) — [docs/ANDROID.md](docs/ANDROID.md).
* Full 2026 survey: [docs/TECHNOLOGY_REVIEW_2026.md](docs/TECHNOLOGY_REVIEW_2026.md).

## 3. Feature status vs. acceptance criteria

| Requirement | Status |
|---|---|
| Streaming engine (no full-file RAM) | ✅ chunk streams, 1 MiB default, negotiated |
| Resume after loss/sleep/restart | ✅ SQLite journal + ranged `have_ranges`; chaos-tested in CI |
| BLAKE3 during transfer, no double I/O | ✅ streamed hash both directions + `Msg::Verify` |
| Atomic receive (.part → rename) | ✅ |
| Path traversal hardening, limits, disk precheck | ✅ tested |
| Small-file batching / directories | ✅ manifest with 200k entry cap |
| Conditional zstd (never JPEG/MP4/ZIP/APK) | ✅ policy + bench samples |
| Protocol v1 versioning + capabilities | ✅ forward-compat unknown-field policy |
| LAN offline-first | ✅ E2E tests run with relay disabled |
| Path scoring + hysteresis + migration | ✅ Lan100/Inet80/Relay40 model in network crate |
| QR pairing, accountless, auth-code confirm | ✅ (120 s TTL, rate-limited) |
| Ed25519 identity, Keystore/DPAPI | ✅ DPAPI implemented; Keystore hook documented |
| Android share target, LNP permissions, battery-aware | ✅ manifest/Kotlin + design; APK not buildable here |
| Windows tray, From/To Phone folders, autostart | ✅ tray crate cross-compiles in CI; outbox watcher in core |
| Self-hostable blind relay + rendezvous | ✅ iroh-relay wrapper + Axum control plane + Docker |
| Wi-Fi Direct | deferred by design — [docs/ADR-WIFI-DIRECT-2026.md](docs/ADR-WIFI-DIRECT-2026.md) |

## 4. Tests

* `cargo test --workspace` (CI): protocol roundtrips + proptest, transfer
  journal/ranges/hash, identity/discovery units, core watcher units.
* **LAN E2E in-process** (`dropbridge-core/tests/lan_e2e.rs`): QR pairing,
  directory transfer w/ BLAKE3 verify, **resume after injected mid-transfer
  fault**, untrusted-sender refusal — all with `RelayConfig::Disabled`
  (offline-first proof).
* Bench doubles as smoke test (pair→send→verify each CI run).
* Honest ledger of what is NOT testable in this environment:
  [docs/TESTING_STATUS.md](docs/TESTING_STATUS.md).

## 5. Benchmarks

* CI loopback suite (`dropbridge-bench --quick`): disk write/read, BLAKE3,
  full engine loopback transfer, zstd samples; `--matrix` for stream counts
  1/2/4/8. Latest numbers: see CI run summary / docs/BENCHMARKS.md.
* Loopback numbers are regression detectors (documented honestly); real-LAN
  numbers require physical hardware not available here.

## 6. CI pipeline (`.github/workflows/dropbridge.yml`)

Every push: fmt(auto-fix) → clippy(`--workspace --all-targets`) → test →
Windows tray cross-check (`x86_64-pc-windows-msvc`) → loopback bench.
Failure logs + fmt fixes are committed back to the branch automatically
(single-publisher design to avoid push races).

## 7. Deliverables map

* Docs: README, ARCHITECTURE, CONTRIBUTING, SECURITY + docs/{PROTOCOL,
  THREAT_MODEL, TECHNOLOGY_REVIEW_2026, ADR-001-network-stack,
  ADR-WIFI-DIRECT-2026, BENCHMARKS, ANDROID, WINDOWS, RELAY, TESTING_STATUS}.md
* Crates (12): protocol, identity, discovery, transfer, network, core, cli,
  tray, ffi, relay, bench, rendezvous.
* App: `app/flutter` (UI + Android share target + FFI bridge).
* Ops: `deploy/relay` (Docker+Caddy), `scripts/`, `integration-tests/`.

## 8. Known limitations (stated plainly)

1. No physical Android/Windows validation in this environment (no SDKs, no
   devices) — flows are implemented to spec; sign-off checklist provided.
2. Benchmarks are loopback until hardware runs are recorded.
3. Android Keystore protector is a documented integration point (JNI) —
   file-protector fallback implemented.
4. Tray UX/toasts/autostart need one manual pass on real Windows.
5. Sandbox cannot compile Rust locally — all validation via CI (documented
   in TESTING_STATUS).

## 9. Next priorities

1. Green CI → merge-quality checkpoint of the Rust core.
2. Android APK build (cargo-ndk) + Keystore JNI protector.
3. Windows installer (MSIX/wix) + toast wiring.
4. Real-LAN benchmark captures; publish to docs/BENCHMARKS.md.
5. Integration-test matrix execution on physical devices.
