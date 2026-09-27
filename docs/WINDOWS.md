# DropBridge on Windows

Goal: **the tray app is the product.** Files dropped into a folder go to the
phone; files from the phone land in a folder. No browser, no browser-upload,
no accounts.

## Components

| Piece | What it is |
|---|---|
| `dropbridge.exe daemon` | the engine (Rust core): endpoint, discovery, transfers |
| `dropbridge-tray.exe` | resident shell: tray icon, menu, autostart, daemon supervision |
| Flutter UI (optional) | full device/transfer/history view (talks to daemon over FFI) |

The tray is intentionally a **thin shell** with no networking dependencies:
it supervises the daemon process, opens folders, and toggles autostart. It
never touches keys or crypto.

Two implementation details matter on Windows and are easy to regress:

* the tray binary is a **GUI** app (`#![cfg_attr(windows, windows_subsystem =
  "windows")]`), so autostart never flashes a black console window and
  startup errors surface as a message box;
* the tray **pumps the Win32 message queue itself** (`PeekMessageW` /
  `TranslateMessage` / `DispatchMessageW`) on the thread that owns the tray
  window. `tray-icon` creates that hidden window on the calling thread and
  subclasses it for the menu, so without a message pump the icon appears but
  the right-click menu never opens. The pump is interleaved with the
  `MenuEvent` channel on one thread because muda menu handles are `!Send`.

## Folders

| Folder | Behavior |
|---|---|
| `%USERPROFILE%\DropBridge\From Phone` | incoming files land here (atomic rename from `.dropbridge-part`) |
| `%USERPROFILE%\DropBridge\To Phone` | drop files here to send to the phone |
| `%USERPROFILE%\DropBridge\logs\dropbridge.log` | tray + engine log (rotated at 4 MiB → `.log.1`) |
| `%USERPROFILE%\.dropbridge\DropBridge\state` | identity (DPAPI-wrapped), trust store, SQLite journal, `hints.json` |
| `%LOCALAPPDATA%\DropBridge` | the installed binaries (default `-InstallDir`) |

The engine's state root is `<base>\DropBridge\state` where `base` is
`%USERPROFILE%\.dropbridge` (override with the `DROPBRIDGE_HOME` environment
variable or `--state`). The doubled `DropBridge` segment is real: the first
one is part of the base directory, the second comes from the engine config
layout. Pass `--state <dir>` if you want a single-level path.

### "To Phone" watcher rules (spec §34)
* File is picked up only after it is **stable**: same size across two polls
  1 s apart and no write activity — avoids grabbing half-copied files.
* Subfolders are preserved as relative paths on the phone.
* After successful verified delivery the file is moved to
  `To Phone\Sent\<date>\` (never deleted silently).
* Collisions on the receiving side get suffixes `-1`, `-2`, … (spec §36).

## Pairing a phone

```powershell
dropbridge daemon          # first run creates identity + folders
dropbridge pair            # shows QR (also available from tray menu)
```

The QR is rendered as plain ASCII (`##` / spaces), because the tray opens
`dropbridge pair` in a console with the OEM code page (cp866/cp1251) where
Unicode block glyphs are mojibake.

Scan with the Android app, confirm the 6-digit code on both sides. Done —
the phone appears under *Trusted devices*.

## Tray menu

* **Open "From Phone"** / **Open "To Phone"** — reveals folders in Explorer.
* **Pair new device…** — opens a console with the pairing QR. The console
  runs detached; the tray menu stays responsive while it is open.
* **Devices** — trusted list with last-seen and path (LAN/P2P/relay).
* **Start with Windows** — toggles `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
  (per-user, no admin required). The checkmark always reflects the real
  registry state, both at start-up and after a toggle.
* **Pause receiving** — stops the daemon politely and unchecks the item;
  toggling again respawns it.
* **Quit** — stops the daemon cleanly (in-flight transfers finish first).

## Stopping the daemon on Windows

There is no signal to send to a detached process, so the tray asks politely
the only way Windows allows: it calls `AttachConsole(<pid>)` and then
`GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0)`. The engine installs a
`SetConsoleCtrlHandler` that records the request and returns `TRUE` (so the
OS does not kill it outright), and the tokio loop turns the flag into a
shutdown future that lets in-flight transfers finish. If the engine ignores
the first request for 10 s the tray escalates: `CTRL_C_EVENT`, then a hard
`TerminateProcess`.

## Notifications

* Nothing is shown as a Windows toast. Transfer and pairing events are
  visible in the daemon console and in the log file
  (`%USERPROFILE%\DropBridge\logs\dropbridge.log`).
* Checksum mismatch never lands a file: the partial is kept as
  `.dropbridge-part.corrupt` for forensics and the sender is told to retry
  (spec §37).

## Explorer context menu

`dropbridge shell install` (called automatically by `install.ps1`) writes
**two** registry surfaces under `HKCU\Software\Classes`:

* the modern Windows 11 surface —
  `CLSID\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\shell\DropBridge` with
  `MultiSelectModel=Player`, which is what makes the item appear in the
  compact right-click menu;
* the classic surfaces — `*\shell\DropBridge`,
  `Directory\shell\DropBridge`, `Directory\Background\shell\DropBridge` —
  which older Windows shows directly and Windows 11 hides behind
  «Показать ещё».

A true multi-select modern verb would need a COM `IExplorerCommand` server
distributed as a sparse package; a zero-install CLI cannot do that, so the
modern entry uses `MultiSelectModel=Player` (send the primary selection).
Registering only the classic keys — which this project used to do — is the
number-one reason for "the menu item does not appear" on Windows 11.

`dropbridge shell status` prints which of the four keys are present.

## Firewall & networking

* Run the daemon with a fixed port (`dropbridge daemon --port 44013`) and
  allow **one** inbound UDP rule:
  `New-NetFirewallRule -DisplayName "DropBridge" -Direction Inbound -Protocol UDP -LocalPort 44013 -Action Allow`
  (Without `--port` the daemon binds an ephemeral port and relies on QUIC
  hole-punching / relay instead of inbound rules.)
* LAN discovery uses mDNS (UDP 5353) — allowed by default on private
  networks; on "public" profiles Windows blocks it and DropBridge falls back
  to manual pairing (paste QR text).
* Internet reachability needs **no** port forwarding: NAT traversal +
  relay are handled by the network layer.

## Key protection (DPAPI)

The Ed25519 identity key is wrapped with `CryptProtectData`
(`CRYPTPROTECT_UI_FORBIDDEN`) and the sealed blob is stored in
`%USERPROFILE%\.dropbridge\DropBridge\state\key-marker.json` (the name is
legacy — the file holds raw DPAPI bytes, nothing parses it as JSON).
Unwrapping succeeds only for the same Windows user account and machine.

`platform_protector()` therefore returns a **chain**: DPAPI first, and a
`FileProtector` read of `state\keys\device.key` as a fallback for state
directories that were sealed by a different protector (older builds, a
copied profile). The chain only ever *writes* with DPAPI. If both fail, the
error names both protectors and says the state directory most likely belongs
to another user or machine — delete it and pair again. See
docs/SECURITY.md for the full model.

## Autostart & lifecycle

* Autostart runs `dropbridge-tray.exe` with no arguments; the menu toggle
  writes/removes the Run value.
* The tray is single-instance: a named mutex (`Local\DropBridgeTray`) makes a
  second launch report "already running" and exit instead of spawning a
  second daemon.
* If the daemon exits or crashes, the tray restarts it with exponential
  backoff (1 s doubling up to 60 s) and logs every attempt.
* Sleep/resume: QUIC connections migrate/reconnect automatically; in-flight
  transfers resume from the journal.
* No background service: everything is per-user; no UAC, no admin.

## Building

```powershell
# engine + CLI + tray
cargo build --release -p dropbridge-cli -p dropbridge-tray
```

Both binaries must ship the **static CRT** (`-C
target-feature=+crt-static`), otherwise a clean Windows 10/11 machine without
the VC++ 2015-2022 Redistributable fails at launch with
`VCRUNTIME140.dll is missing` (exit code `0xC0000135`). The flag lives in
`.cargo/config.toml` and is also set explicitly in the release workflow; the
release job fails the build if a `VCRUNTIME140.dll`/`api-ms-win-crt-` import
reappears in either PE.

Cross-compiling from macOS/Linux needs `cargo-xwin`:
`cargo xwin build --release -p dropbridge-cli -p dropbridge-tray`.

Note that `dropbridge-tray` is deliberately excluded from the workspace
`default-members` (it only compiles on Windows), so a bare
`cargo build --release` does not produce it — pass `-p dropbridge-tray`.

## Platform limits

* `TransferSource::Fds` (raw file descriptors, used by the FFI receive path)
  has no Windows implementation: there is no `/proc/self/fd`. `build_manifest`
  now returns `PlanError::UnsupportedFdSource` instead of pointing at a path
  that cannot exist. Pass file paths from Windows callers.
* The pre-flight free-space probe uses `GetDiskFreeSpaceExW` and **fails
  closed** — a probe error aborts the receive instead of assuming infinite
  space.
* `nix`/`libc` are Unix-only dependencies of the transfer crate and are
  target-gated, so a Windows build does not carry them.

## Status (honest)

* Engine, tray shell, DPAPI protector (with fallback), outbox watcher
  (`dropbridge_core::watcher`, started by `dropbridge daemon`): implemented
  in Rust. The whole workspace is compiled and linted for
  `x86_64-pc-windows-msvc` in CI on a Windows runner, and the CI job also
  launches `dropbridge.exe --version` and asserts the tray PE subsystem is
  `WINDOWS_GUI`.
* **Still needs a manual pass on a physical Windows machine**: tray icon
  appearance, firewall prompts, and the actual pairing round-trip. Tracked in
  docs/TESTING_STATUS.md.
* Toast notifications are NOT implemented (see above).
