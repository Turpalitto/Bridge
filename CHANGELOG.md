# Changelog

All notable changes to DropBridge. Format follows Keep a Changelog;
versions follow SemVer. Protocol compatibility is tracked separately in
`dropbridge-protocol` (`PROTOCOL_VERSION`).

## [Unreleased]

### Fixed
- **Windows launch blocker #1 — missing VC++ runtime.** Shipped binaries now
  statically link the CRT. The flag lives in `.cargo/config.toml` (previously
  untracked, so CI never had it) *and* is set explicitly as
  `RUSTFLAGS: -C target-feature=+crt-static` in the release workflow. The
  release job now reads each produced PE and **fails the build** if a
  `VCRUNTIME140.dll` / `VCRUNTIME140_1.dll` / `MSVCP140.dll` /
  `api-ms-win-crt-*` import reappears, and it smoke-tests
  `dropbridge.exe --version`. Before this, a clean Windows 10/11 machine died
  with `VCRUNTIME140.dll is missing` (exit `0xC0000135`) — the "it does not
  even start" report.
- **Windows launch blocker #2 — the tray had no message pump.**
  `tray-icon` creates its hidden window on the *calling* thread and `muda`
  subclasses it, so the Win32 message queue must be pumped there. The tray
  blocked in `MenuEvent::recv()` instead, so `Shell_NotifyIconW` showed the
  icon but the right-click menu never opened — a visible, inert tray. The
  loop now interleaves `PeekMessageW`/`TranslateMessage`/`DispatchMessageW`
  with the `MenuEvent` channel (muda handles are `!Send`, so both must share
  one thread).
- **Tray is a GUI subsystem binary.** `#![cfg_attr(windows,
  windows_subsystem = "windows")]` — the console subsystem made autostart
  flash a black window for the process lifetime. Startup errors are now shown
  with `MessageBoxW` instead of disappearing into a console nobody sees.
- **Tray: single instance.** A named mutex (`Local\DropBridgeTray`) prevents
  a second launch from spawning a second daemon; it now reports "already
  running" and exits.
- **Daemon failures were invisible.** `spawn_daemon` used `Stdio::null()` and
  `.spawn().ok()`, and nothing restarted it. The tray now writes tray *and*
  engine output to `%USERPROFILE%\DropBridge\logs\dropbridge.log` (rotated at
  4 MiB) and supervises the daemon with exponential backoff (1 s → 60 s).
  `docs/WINDOWS.md` promised this supervision; it did not exist.
- **Polite daemon stop on Windows.** `taskkill /PID` without `/F` was useless
  against a `CREATE_NO_WINDOW` engine (no top-level window to receive
  `WM_CLOSE`). The tray now `AttachConsole`es to the child and sends
  `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)`, escalating to `CTRL_C_EVENT`
  and finally a hard kill after 10 s each.
- **Windows graceful shutdown in the engine.** New `dropbridge-cli`
  `shutdown` module: on Unix SIGTERM/SIGINT, on Windows
  `SetConsoleCtrlHandler` recording the event into an atomic that a tokio
  poller turns into a shutdown future, so console close and the tray's
  `CTRL_BREAK` both let in-flight transfers finish. Previously the
  `#[cfg(not(unix))]` select branch awaited `std::future::pending()` and
  nothing on Windows could ever stop the daemon politely.
- **DPAPI now has a fallback.** `platform_protector()` on Windows returns a
  `ChainedProtector` — DPAPI first, then a read-only `FileProtector` for
  state sealed by another protector. It only ever *writes* with DPAPI, and a
  double failure produces an error naming both protectors and the likely
  cause (state directory owned by a different Windows user/machine).
  `CryptProtectData`/`CryptUnprotectData` errors now include the Win32 code.
- **Win11 context menu was invisible.** Only the classic
  `HKCU\Software\Classes\*\shell\DropBridge` keys were registered, so
  Windows 11 hid the item behind «Показать ещё». `dropbridge shell install`
  now also writes the modern surface
  `CLSID\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\shell\DropBridge` with
  `MultiSelectModel=Player`, plus
  `Directory\shell` and `Directory\Background\shell`. `shell status` reports
  which keys exist. `reg.exe` is now spawned with `CREATE_NO_WINDOW`, so
  installing from the tray no longer flashes four consoles.
- **QR was mojibake on Windows.** `dropbridge pair` rendered the code with
  Unicode half-block glyphs; the tray opens it in an OEM-code-page console.
  The renderer is now pure ASCII (`##` / space), covered by a test asserting
  the output is ASCII and square.
- **Installer could abort halfway.** `$ErrorActionPreference = "Stop"` with
  no per-step `try/catch` meant any failure in the context-menu step killed
  the script before autostart, the Start Menu shortcut and the tray launch —
  leaving a half-configured system. Every step is now isolated, failures are
  collected and printed in a final summary with manual retry commands, and a
  failed `dropbridge.exe --version` is a **hard gate** that stops the install
  instead of registering a binary that cannot start. PATH detection no longer
  false-positives on a substring match.
- **Tray menu no longer freezes.** `run_core_console` used `.status()`,
  blocking the single-threaded event loop until the child exited; pair /
  devices / send now spawn detached consoles and are reaped in the loop.
- **Tray checkmarks now tell the truth.** Autostart and Pause are
  `CheckMenuItem`s initialised from the registry and updated after every
  toggle, instead of a static unchecked box.
- **Windows disk probe is now fail-closed.** `free_space` returned
  `u64::MAX` when `GetDiskFreeSpaceExW` failed, silently passing the
  pre-flight check; it now propagates the OS error and uses `windows-sys`
  instead of a hand-declared `extern "system"`.
- **`TransferSource::Fds` is honest on Windows.** It mapped to
  `/proc/self/fd/{fd}`, a path that cannot exist there; `build_manifest` now
  returns `PlanError::UnsupportedFdSource` with a test on both platforms.
- **CI actually compiles the Windows code.** `windows-check` ran
  `cargo check -p dropbridge-tray` on a Linux runner, so the engine CLI's
  Windows-only paths (DPAPI, the disk probe, the console handler, the
  registry verbs) were never compiled by CI. It now runs on `windows-latest`
  (bundled SQLite needs a real MSVC toolchain) and does
  `cargo check` + `cargo clippy -D warnings` for the **whole workspace**,
  then builds both binaries, runs `dropbridge.exe --version` and asserts the
  tray PE subsystem is `WINDOWS_GUI`.
- `docs/WINDOWS.md` corrected: the state directory is
  `%USERPROFILE%\.dropbridge\DropBridge\state` and the key file is
  `key-marker.json` (not `%LOCALAPPDATA%\DropBridge\state` /
  `identity.key`); the toast notifications, the `--autostart` argument and
  the WM_CLOSE-based stop it described never existed and are gone.
- `scripts/build-windows.ps1` and `scripts/package-windows.ps1` no longer
  disagree with the release pipeline: both build the two binaries in a single
  `cargo` invocation, accept `-Target` for `cargo-xwin` cross-builds, force
  `+crt-static` for Windows targets, and the packager emits the real artifact
  name `DropBridge-Windows-x64.zip` plus its `.sha256`. The packager also
  stages the same five files the release job ships (engine, tray, installer,
  uninstaller, readme) and refuses to produce an archive whose tray binary is
  not `WINDOWS_GUI`.

### Removed
- `windows/sparse/AppxManifest.xml` and the copy in `dist/windows/` — orphaned
  since the sparse-package install step was dropped and no workflow, script or
  document referenced either file.

### Changed
- `dropbridge-tray` is back in the workspace `default-members`. It builds on
  every platform through its non-Windows `main()`, so a bare `cargo build` /
  `cargo test` / `cargo clippy` at the repo root now compiles and lints the
  tray sources too instead of silently skipping them.
- Windows package no longer ships the sparse `AppxManifest.xml` (the COM verb
  server is not implemented yet; the modern CLSID shell key is registered in
  the registry instead and needs no package registration).
- `nix` and `libc` are now target-gated to Unix in `dropbridge-transfer`, so
  a Windows build no longer compiles two Unix-only crates it never calls.
- Android: runtime permissions (NEARBY_WIFI_DEVICES, POST_NOTIFICATIONS) are
  requested at startup; the Quick Settings tile actually toggles receive mode;
  release APK signing comes from `android/key.properties` (example provided)
  with a debug-key fallback for local builds.

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
