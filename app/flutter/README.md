# DropBridge Flutter shell

Thin UI over the Rust core (`rust/crates/dropbridge-ffi`). Dart never sees
file bytes — only commands, metadata and events.

## Layout

```
lib/main.dart          UI: devices, pairing, send, event log
lib/ffi_bridge.dart    dart:ffi bindings for the C-ABI (db_* functions)
android/               manifest (share target, LNP permissions), Kotlin:
                       MainActivity (channel), ShareEntryActivity (staging),
                       TransferForegroundService (active transfers only)
```

## Build

```bash
# 1. build the Rust core as a shared library for the ABI(s)
cargo install cargo-ndk   # once
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 \
  -o android/app/src/main/jniLibs build --release -p dropbridge-ffi

# 2. flutter side
flutter pub get
flutter build apk          # or: flutter run
```

Windows desktop: `cargo build --release -p dropbridge-ffi` +
`flutter build windows` (copy `dropbridge_ffi.dll` next to the exe).

## Notes

* `flutter create .` can regenerate missing platform boilerplate; the files
  checked in here carry the DropBridge-specific parts (share target,
  permissions, channels) and win over generated defaults.
* The FFI surface is documented in `rust/crates/dropbridge-ffi/src/lib.rs`.
* `flutter_rust_bridge` remains an optional upgrade path; the C-ABI JSON
  surface was chosen to keep the security boundary auditable (see
  docs/ANDROID.md).
