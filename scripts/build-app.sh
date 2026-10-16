#!/bin/bash
# Assembles ~/Applications/Awake.app from the release binary and ad-hoc signs it.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
APP="$HOME/Applications/Awake.app"

echo "==> Building awake-app (release)"
(cd "$REPO" && cargo build --release -p awake-app)

echo "==> Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$REPO/packaging/Info.plist" "$APP/Contents/Info.plist"
cp "$REPO/target/release/awake-app" "$APP/Contents/MacOS/awake-app"
chmod 755 "$APP/Contents/MacOS/awake-app"
codesign --force --sign - "$APP"
echo "built $APP"
