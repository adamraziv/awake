#!/bin/bash
# Assembles ~/Applications/Awake.app from the release binaries and ad-hoc signs it.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
APP="$HOME/Applications/Awake.app"
HELPER_LABEL="com.voltron.awake-helper"

echo "==> Building awake-app and awake-helper (release)"
(cd "$REPO" && cargo build --release -p awake-app -p awake-helper)

echo "==> Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Library/LaunchServices"
cp "$REPO/packaging/Info.plist" "$APP/Contents/Info.plist"
cp "$REPO/target/release/awake-app" "$APP/Contents/MacOS/awake-app"
chmod 755 "$APP/Contents/MacOS/awake-app"
# Not run from here; install.sh installs the helper from target/. The copy follows the
# SMJobBless layout so cleaners that look for an owning app (Mole's `mo clean`) see the
# installed helper as Awake's and leave it alone.
cp "$REPO/target/release/awake-helper" "$APP/Contents/Library/LaunchServices/$HELPER_LABEL"
chmod 755 "$APP/Contents/Library/LaunchServices/$HELPER_LABEL"
# Nested code is signed before the bundle that contains it.
codesign --force --sign - "$APP/Contents/Library/LaunchServices/$HELPER_LABEL"
codesign --force --sign - "$APP"
echo "built $APP"
