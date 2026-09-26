# Contributing to DropBridge

## Ground rules

1. **No placeholders on critical paths.** If you can't implement it, say so
   in docs/TESTING_STATUS.md — don't leave a `todo!()` in transfer/security
   code paths.
2. **Benchmark, don't guess.** Any change to chunk size, stream count,
   compression triggers or path scoring must come with `dropbridge-bench`
   numbers (before/after) in the PR description.
3. **Relay stays blind.** No change may give rendezvous/relay access to
   plaintext. Security regressions are release blockers.
4. **Protocol changes are versioned.** `dropbridge-protocol` bumps the
   version field; old peers must fail with a clear "unsupported version",
   not corrupt data (spec §49).

## Layout recap

Rust workspace root is `dropbridge/Cargo.toml` (crates under
`rust/crates/*`, plus `server/rendezvous`). Flutter app lives in
`app/flutter`. CI: `.github/workflows/dropbridge.yml`.

## Dev workflow

```bash
cd dropbridge
cargo fmt --all                # CI runs this and commits fixes back
cargo clippy --workspace --all-targets
cargo test --workspace         # includes in-process LAN E2E tests
cargo run -p dropbridge-bench -- --quick    # smoke + numbers
```

### Running two local nodes (manual E2E)

```bash
# terminal 1 (receiver)
cargo run -p dropbridge-cli -- --state /tmp/db-a --name laptop \
  --relay disabled daemon --port 44013
# note the QR from: cargo run -p dropbridge-cli -- --state /tmp/db-a pair

# terminal 2 (sender)
cargo run -p dropbridge-cli -- --state /tmp/db-b --name phone \
  --relay disabled join "<qr text>"
cargo run -p dropbridge-cli -- --state /tmp/db-b --name phone \
  --relay disabled send laptop ./some-files
```

`--relay disabled` keeps the test fully LAN/offline.

### Windows tray

```bash
cargo check -p dropbridge-tray --target x86_64-pc-windows-msvc  # cross-check
# actual run needs Windows: cargo run -p dropbridge-tray
```

The tray is intentionally dependency-free of the core crate (it supervises
the `dropbridge` daemon process) so it cross-compiles from Linux CI.

## Testing expectations

* New engine behavior → unit test in the owning crate.
* New flow → extend `dropbridge-core/tests/lan_e2e.rs` (in-process LAN
  flows; deterministic, no real network required).
* Fault paths → use existing hooks (`test_recv_fail_after`) rather than
  `sleep()`-based races.
* Anything you cannot test → document it in docs/TESTING_STATUS.md.

## Commit style

Conventional commits (`feat(scope):`, `fix(scope):`, `docs:`, `bench:`…).
Each phase checkpoint is one squash-friendly series; keep commits building
(CI runs on every push).

## Benchmark submissions

`cargo run -p dropbridge-bench --release -- --matrix --dir /tmp/dbbench`
captures disk/hash/loopback numbers; paste the Markdown table into
docs/BENCHMARKS.md with hardware description + date. Real-LAN numbers
(two physical devices) are the gold standard — loopback numbers are
regression detectors only.
