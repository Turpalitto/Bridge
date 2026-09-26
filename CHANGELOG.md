# Changelog

All notable changes to DropBridge. Format follows Keep a Changelog;
versions follow SemVer. Protocol compatibility is tracked separately in
`dropbridge-protocol` (`PROTOCOL_VERSION`).

## [0.1.0] - 2026-09-24

First feature-complete milestone: engine, pairing, platforms shells,
servers, docs.

### Added
- **Transfer engine**: streaming chunk transport (no full-file RAM),
  adaptive negotiated chunk size (64 KiB–16 MiB) and stream count (1–16),
  persistent SQLite-WAL journal, ranged resume after loss/sleep/restart,
  streamed BLAKE3 verification (`Msg::Verify`), atomic receive
  (`.dropbridge-part` → rename), small-file batching via manifest,
  directory transfers, collision-safe naming, disk-space precheck,
  resource limits (frames, entries, concurrency).
- **Networking**: iroh 1.2.0 QUIC endpoints; LAN direct / Internet P2P /
  encrypted blind relay path selection with scoring + ×1.15 hysteresis;
  QUIC migration & resume; fully offline LAN mode.
- **Identity & pairing**: Ed25519 device identity; accountless QR pairing
  with one-time token (120 s TTL) + 6-digit human-confirmed auth code;
  trust registry with per-device permissions; DPAPI key protection on
  Windows, protector trait for Android Keystore integration.
- **Protocol v1**: postcard-framed messages, capability negotiation,
  forward-compatible unknown-field policy, documented limits.
- **Windows**: resident tray shell (menu, autostart via HKCU, daemon
  supervision), `From Phone` auto-receive folder, `To Phone` outbox
  watcher with file-stability detection and `Sent/<date>` archiving.
- **Android**: Flutter app shell with share target (SEND/SEND_MULTIPLE,
  text/URLs), Local Network Protection permission flows, FGS only during
  active transfers, C-ABI FFI bridge.
- **Servers**: self-hostable blind relay (iroh-relay wrapper, Docker +
  Caddy TLS, rate limits, Prometheus metrics) and minimal Axum rendezvous
  (signed presence + wake hints, SQLite-backed).
- **Benchmarks**: `dropbridge-bench` (disk/hash/loopback/stream matrix/
  zstd samples) wired into CI.
- **CI**: fmt/clippy/test + Windows cross-check + loopback bench on every
  push, with failure logs published back to the branch.
- **Docs**: architecture, protocol spec, security + threat model, ADRs
  (network stack, Wi-Fi Direct), platform guides, relay ops, testing
  status ledger.

### Known limitations (see docs/TESTING_STATUS.md)
- Physical-device validation (Android APK run, Windows tray UX) pending.
- Real-LAN benchmark numbers pending hardware access.
- flutter_rust_bridge codegen optional; C-ABI JSON surface is primary.
