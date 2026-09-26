# DropBridge on Android

Goal: **Share → DropBridge → device. Done.** The app is a share target first
and a browser of devices second.

## UX contract

1. User hits Share in any app → DropBridge appears.
2. Device picker shows trusted devices with live path badges
   (LAN / P2P / relay) and last-seen.
3. Tap target → progress → "Delivered ✓". No account, no cloud, no waiting
   for uploads.

Incoming: notification "Files from Laptop ✓", tap opens the receive folder
(`Android/media/DropBridge/From Laptop` or `Documents/DropBridge` depending
on API level scoped-storage rules).

## Architecture

```
Kotlin UI (Flutter)  ── commands/events (JSON over FFI) ──►  Rust core (same engine as Windows)
   │ shares: content:// URIs, multi-item, text, URLs
   ▼
ShareTargetService → content-resolver copies URIs into a staged area
   (only metadata crosses to Dart; bytes stream through Rust via fds)
```

* **No eternal background service** (spec §27). The engine runs while the
  app is foreground/FGS; when idle it stops. Reaching a stopped phone uses
  the rendezvous *wake* path (§ below).
* File descriptors (`ParcelFileDescriptor`) for shared content are handed to
  Rust through the FFI so large videos never transit Dart memory.

## Share target (manifest essentials)

```xml
<activity android:name=".share.ShareEntryActivity" android:exported="true">
  <intent-filter>
    <action android:name="android.intent.action.SEND" />
    <action android:name="android.intent.action.SEND_MULTIPLE" />
    <category android:name="android.intent.category.DEFAULT" />
    <data android:mimeType="*/*" />
  </intent-filter>
</activity>
```

Handled inputs: single/multiple files (`content://`), `text/plain`
(saved as `.txt`), URLs (saved as `.url` stub + optional fetch), and mixed
SEND_MULTIPLE clips. Directory picks go through SAF (`ACTION_OPEN_DOCUMENT_TREE`).

## Permissions & Android 16 Local Network Protection

| Permission | When | Why |
|---|---|---|
| `NEARBY_WIFI_DEVICES` (API 33+) | runtime, on first LAN transfer | mDNS discovery + direct QUIC on Wi-Fi |
| `ACCESS_LOCAL_NETWORK` (API 37+, dangerous) | runtime on Android 16+ | mandatory for LAN sockets under LNP; enforcement window 2025Q2→2026Q2 |
| `POST_NOTIFICATIONS` (API 33+) | runtime | transfer/pairing notifications |
| `INTERNET` | install | P2P/relay paths |
| `FOREGROUND_SERVICE_DATA_SYNC` | FGS type for active transfers only | survive app switch mid-transfer |

Key behaviors:
* If the user denies the local-network permission we degrade gracefully:
  Internet P2P/relay still works; UI explains why LAN is unavailable.
* Discovery is **battery-aware** (spec §28): mDNS browsing in bounded
  bursts, paused below 15% battery (unless charging), never continuous.
* `NsdManager` runs in its own process exemption path where available;
  our primary discovery is mdns-sd in Rust which keeps its own socket.

## Wake path (receiving while "closed")

No push payload ever contains file data (spec §57–58):

```
sender sees target offline → rendezvous POST /v1/wake {device, ticket}
  → phone's FCM high-priority data message (payload = ticket id only)
  → app starts a short FGS → engine connects back to sender (E2E QUIC)
  → transfer proceeds; FGS ends when done.
```

## Keystore

The Ed25519 identity lives in **Android Keystore** (StrongBox when
available): the core exposes a protector trait; the Android build plugs a
Keystore-backed protector in via FFI so the raw key never exists in app
files. Fallback (no Keystore / emulator): file protector with
`EncryptedSharedPreferences` wrapper — flagged in the security doc.

## Building

```bash
cd app/flutter          # Flutter app (UI + share UX)
flutter pub get
# Rust core → .so via cargo-ndk (arm64-v8a + armeabi-v7a + x86_64):
cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build --release -p dropbridge-ffi
flutter build apk
```

The FFI layer (`rust/crates/dropbridge-ffi`) is a C-ABI JSON surface — the
same surface used by the Windows shell — so Dart/Kotlin never link anything
beyond `libdropbridge_ffi.so`. `flutter_rust_bridge` remains an optional
codegen upgrade path; see the FFI crate docs.

## Status (honest)

* Core engine + share staging design + FFI surface: implemented in Rust.
* APK build, Keystore protector, FCM wake and LNP permission flows:
  **not yet runnable in this environment** (no Android SDK here). The
  manifest, permission matrix and flows above are the implementation spec;
  gaps are tracked in docs/TESTING_STATUS.md.
