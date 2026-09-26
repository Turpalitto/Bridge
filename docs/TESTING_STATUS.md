# Testing & Verification Status

Honest ledger of what is verified and how. Updated each phase.

## Verified in CI (every push)

| Check | How |
|---|---|
| `cargo fmt` | auto-format step |
| `cargo clippy --workspace --all-targets` | errors fail the build |
| `cargo test --workspace` | unit + integration tests below |
| `dropbridge-tray` cross-check | `cargo check --target x86_64-pc-windows-msvc` on Ubuntu runner — **green** |
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

* Windows tray UX (menu, autostart toggle, daemon supervision) — code
  cross-compiles; needs a physical Windows machine for final sign-off.
* DPAPI protect/unprotect — algorithm follows MSDN; needs Windows run.
* Relay under load — wrapper around iroh-relay (battle-tested upstream);
  our rate-limit flag not load-tested yet.

## NOT verified (no environment available in this workspace)

Stated plainly per project rules — these are designed but unexecuted:

* **Physical Android device**: share-sheet flow, LNP permission grants,
  Keystore key generation, FCM wake. No Android SDK/emulator here.
* **Physical Windows machine**: tray behavior, firewall prompt, toast
  notifications, autostart. No Windows runner in this sandbox.
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
