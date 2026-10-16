#!/bin/bash
set -euo pipefail

LABEL="com.voltron.awake-helper"
HELPER_DST="/Library/PrivilegedHelperTools/$LABEL"
PLIST_DST="/Library/LaunchDaemons/$LABEL.plist"

echo "==> Stopping and removing Awake.app"
pkill -x awake-app || true
rm -rf "$HOME/Applications/Awake.app"
echo "If 'Awake' remains under System Settings > General > Login Items, remove it there."

echo "==> Resetting sleep setting (sudo required)"
sudo /usr/bin/pmset -a disablesleep 0 || true

echo "==> Removing helper"
if sudo launchctl print "system/$LABEL" >/dev/null 2>&1; then
    sudo launchctl bootout "system/$LABEL" || true
fi
sudo rm -f "$PLIST_DST" "$HELPER_DST" /var/run/awake-helper.sock
echo "helper removed. Log left at /var/log/awake-helper.log"
