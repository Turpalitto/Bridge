#!/usr/bin/env bash
# Build libdropbridge_ffi.so for Android ABIs into the Flutter jniLibs dir.
# Requires: rustup targets + cargo-ndk (`cargo install cargo-ndk`).
set -euo pipefail
cd "$(dirname "$0")/.."

ABIS=(arm64-v8a armeabi-v7a x86_64)
OUT=app/flutter/android/app/src/main/jniLibs

for abi in "${ABIS[@]}"; do
  echo ">> $abi"
  cargo ndk -t "$abi" -o "$OUT" build --release -p dropbridge-ffi
done
echo "done: $OUT"
