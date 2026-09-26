#!/usr/bin/env bash
# Full Android release build: native libs + Flutter APK.
set -euo pipefail
cd "$(dirname "$0")/.."

./scripts/build-android-so.sh

cd app/flutter
flutter pub get
flutter build apk --release
echo "APK: build/app/outputs/flutter-apk/app-release.apk"
