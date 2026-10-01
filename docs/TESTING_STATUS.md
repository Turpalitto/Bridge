# Testing & Verification Status

Honest ledger of what is verified and how. Updated each phase.

## Verified in CI (every push)

| Check | How |
|---|---|
| `cargo fmt` | auto-format step |
| `cargo clippy --workspace --all-targets` | errors fail the build |
| `cargo test --workspace` | unit + integration tests below |
| Windows build of the **whole workspace** | `cargo check` + `cargo clippy -D warnings --workspace --all-targets --target x86_64-pc-windows-msvc` on a `windows-latest` runner |
| Windows binaries actually launch | CI builds `-p dropbridge-cli -p dropbridge-tray`, runs `dropbridge.exe --version`, and asserts the tray PE subsystem is `WINDOWS_GUI` (2) |
| Static CRT in release artifacts | the release job scans both PE files for `VCRUNTIME140*`/`MSVCP140`/`api-ms-win-crt-` imports and fails if any is present |
| loopback benchmark | `dropbridge-bench --quick` (numbers in run summary) |

CI note: this sandbox cannot compile Rust (no toolchain, crates.io
unreachable), so GitHub Actions is the sole compiler. During bring-up a
series of cross-target type errors were fixed one CI cycle at a time
(journal Mutex, i64 SQL params, rand 0.10 API, network↔transfer dep,
BytesMut conversion, proptest format capture, error-mapping variants,
Windows HKEY types). The ledger above reflects the current pipeline.

### Test inventory (Rust)

* **dropbridge-protocol** — message roundtrips, framing caps, manifest/path
  sanitization (path-traversal cases), property tests.
* **dropbridge-transfer** — journal idempotence, range resume math, chunk
  plan construction, collision policy, BLAKE3 digest correctness,
  `Recv::ingest(fail_after)` chaos hook (deterministic mid-transfer fault).
* **dropbridge-identity** — key roundtrip, protector encrypt/decrypt,
  pairing token/code derivation, trust registry persistence.
* **dropbridge-discovery** — beacon sign/verify, bounded browse envelopes.
* **dropbridge-core (`tests/lan_e2e.rs`)** — real in-process LAN flows:
  1. QR pair → trusted both sides
  2. directory transfer with subfolders, BLAKE3 verify, atomic rename
  3. **resume after fault** (receiver dies mid-transfer via chaos hook;
     same session resumes and completes)
  4. untrusted sender refused
* **dropbridge-bench** doubles as a smoke test: full pair→send→verify on
  every CI run.

## Verified manually / by inspection only

* Windows tray UX (menu opens, autostart toggle, daemon supervision,
  log file) — compiles and is smoke-tested for launch in CI; the visual
  result and the real autostart round-trip still need a physical machine.
* DPAPI protect/unprotect — algorithm follows MSDN; the protector *chain*
  (DPAPI → file fallback) is unit-tested cross-platform, but the DPAPI
  branch itself needs a Windows run.
* Relay under load — wrapper around iroh-relay (battle-tested upstream);
  our rate-limit flag not load-tested yet.

## NOT verified (no environment available in this workspace)

Stated plainly per project rules — these are designed but unexecuted:

* **Physical Android device**: share-sheet flow, LNP permission grants,
  Keystore key generation, FCM wake. No Android SDK/emulator here.
* **Physical Windows machine**: firewall prompt, the actual pairing
  round-trip from the tray, and the autostart round-trip. CI has a Windows
  *runner*, so compile/launch are covered; a human on real hardware is
  not. Toast notifications do not exist — they are not a pending item.
* **True cross-NAT P2P**: CI runs loopback; hole-punch success rate relies
  on iroh's published measurements (≈90%) until we test against real NATs.
* **Wi-Fi Direct** — intentionally deferred (ADR-WIFI-DIRECT-2026).

## Known limitations

* Loopback benchmark numbers overstate real LAN throughput (no NIC queueing,
  same-page cache effects). Treat them as regression detectors, not
  marketing. Real-LAN numbers to be appended to docs/BENCHMARKS.md from
  physical hardware.
* `cargo fmt` in CI mutates the tree and commits back — contributors should
  run `cargo fmt` locally to avoid surprise commits.
