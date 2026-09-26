#!/usr/bin/env bash
# Build all DropBridge release artifacts for the current host + checksums.
# For Windows artifacts use scripts/build-windows.ps1 on Windows (or CI).
# For Android use scripts/build-android-so.sh then `flutter build apk`.
set -euo pipefail
cd "$(dirname "$0")/.."

echo ">> cargo build --release (workspace bins)"
cargo build --release -p dropbridge-cli -p dropbridge-relay -p dropbridge-rendezvous -p dropbridge-bench

DIST=dist/release-$(date +%Y%m%d)
mkdir -p "$DIST"
for bin in dropbridge dropbridge-relay dropbridge-rendezvous dropbridge-bench; do
  cp "target/release/$bin" "$DIST/"
done
(cd "$DIST" && sha256sum * > SHA256SUMS.txt)
echo ">> artifacts in $DIST:"
ls -la "$DIST"
