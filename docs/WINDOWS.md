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
it supervises the daemon process, opens folders, and toggles autostart.
This keeps the resident binary tiny and auditable — and it cross-compiles
cleanly.

## Folders

| Folder | Behavior |
|---|---|
| `%USERPROFILE%\DropBridge\From Phone` | incoming files land here (atomic rename from `.dropbridge-part`) |
| `%USERPROFILE%\DropBridge\To Phone` | drop files here to send to the phone |
| `%LOCALAPPDATA%\DropBridge\state` | identity keys (DPAPI-wrapped), trust store, SQLite journal |

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

Scan with the Android app, confirm the 6-digit code on both sides. Done —
the phone appears under *Trusted devices*.

## Tray menu

* **Open "From Phone"** / **Open "To Phone"** — reveals folders in Explorer.
* **Pair new device…** — shows the QR window.
* **Devices** — trusted list with last-seen and path (LAN/P2P/relay).
* **Start with Windows** — toggles `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
  (per-user, no admin required).
* **Pause receiving** — stops auto-accept (transfers stay queued).
* **Quit** — stops the daemon cleanly (in-flight transfers finish first).

## Notifications

* Transfer complete → Windows toast with file name + *Open folder* action.
* Pairing request → toast with *Approve / Deny* actions.
* Failures (no route, disk full, checksum mismatch) → toast with reason.
Checksum mismatch never lands a file: the partial is kept as
`.dropbridge-part.corrupt` for forensics and the sender is told to retry
(spec §37).

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
(`CRYPTPROTECT_UI_FORBIDDEN`) and stored in
`%LOCALAPPDATA%\DropBridge\state\identity.key`. Unwrapping succeeds only for
the same Windows user account — file copy to another machine or account does
not yield a usable key. See docs/SECURITY.md for the full model.

## Autostart & lifecycle

* Autostart runs `dropbridge-tray.exe --autostart` (menu toggle writes the
  Run key; uninstaller/silent mode removes it).
* If the daemon exits/crashes, the tray restarts it with backoff (visible in
  tray tooltip state).
* Sleep/resume: QUIC connections migrate/reconnect automatically; in-flight
  transfers resume from the journal.
* No background service: everything is per-user; no UAC, no admin.

## Building

```powershell
# engine + CLI (native)
cargo build --release -p dropbridge-cli

# tray (must be built on Windows or cross-compiled for windows-msvc)
cargo build --release -p dropbridge-tray

# Flutter desktop shell (after flutter_rust_bridge or FFI wiring, see app/)
flutter build windows
```

CI cross-checks `dropbridge-tray` for `x86_64-pc-windows-msvc` on every push
(`cargo check`) so Windows build breaks are caught on Linux runners.

## Status (honest)

* Engine, tray shell, DPAPI protector, outbox watcher
  (`dropbridge_core::watcher`, started by `dropbridge daemon`): implemented
  in Rust; tray cross-compiles in CI.
* **Not yet validated on a physical Windows machine** in this environment —
  tray UX, toast rendering, firewall prompts and autostart behavior need a
  manual pass; tracked in docs/TESTING_STATUS.md.
