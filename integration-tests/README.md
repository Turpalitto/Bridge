# Integration tests

Two layers:

1. **In-process LAN E2E** — lives in
   `rust/crates/dropbridge-core/tests/lan_e2e.rs` and runs in CI on every
   push (pairing, directory transfer, resume-after-fault, untrusted
   refusal). No real network needed: two iroh endpoints in one process with
   `RelayConfig::Disabled`.

2. **Two-host plans** (this directory) — checklists + helper script for
   real hardware. These cannot run in the current sandbox (no second
   device, no Android SDK); they are the acceptance procedure for physical
   sign-off and are tracked honestly in `docs/TESTING_STATUS.md`.

## Files

* `run_lan_smoke.sh` — single-machine smoke: two CLI nodes, QR pairing via
  string, directory send, verify. Runs anywhere cargo runs.
* `plan-android-windows.md` — the full manual matrix (LAN, P2P, relay,
  sleep/resume, permissions, share sheet, outbox).

## Running the scripted smoke

```bash
./integration-tests/run_lan_smoke.sh
```

Exit code 0 = paired, transferred, verified, resumed.
