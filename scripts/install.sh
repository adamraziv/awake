#!/bin/bash
# Installs the awake root helper. Phase 2 adds the app bundle below the helper block.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
LABEL="com.voltron.awake-helper"
HELPER_DST="/Library/PrivilegedHelperTools/$LABEL"
PLIST_DST="/Library/LaunchDaemons/$LABEL.plist"
PLIST_SRC="$REPO/packaging/$LABEL.plist"
SOCKET="/var/run/awake-helper.sock"
UID_NUM="$(id -u)"

if [ "$UID_NUM" -eq 0 ]; then
    echo "run as your normal user, not with sudo; the script asks for sudo itself" >&2
    exit 1
fi

echo "==> Building release binaries"
(cd "$REPO" && cargo build --release -p awake-helper)

# ---- helper ----
echo "==> Installing helper (sudo required)"
sudo mkdir -p /Library/PrivilegedHelperTools
if sudo launchctl print "system/$LABEL" >/dev/null 2>&1; then
    sudo launchctl bootout "system/$LABEL" || true
    # bootstrap fails with "Input/output error" if the old job is still unloading
    for _ in $(seq 1 20); do
        sudo launchctl print "system/$LABEL" >/dev/null 2>&1 || break
        sleep 0.25
    done
fi
sudo install -o root -g wheel -m 755 "$REPO/target/release/awake-helper" "$HELPER_DST"
sed "s/__UID__/$UID_NUM/" "$PLIST_SRC" | sudo tee "$PLIST_DST" >/dev/null
sudo chown root:wheel "$PLIST_DST"
sudo chmod 644 "$PLIST_DST"
sudo touch /var/log/awake-helper.log
# a stale socket from the previous helper would satisfy the wait below too early
sudo rm -f "$SOCKET"
sudo launchctl bootstrap system "$PLIST_DST"

echo "==> Waiting for socket"
for _ in $(seq 1 20); do
    [ -S "$SOCKET" ] && break
    sleep 0.25
done
if [ ! -S "$SOCKET" ]; then
    echo "helper socket did not appear; see /var/log/awake-helper.log" >&2
    exit 1
fi

echo "==> Smoke test"
# -w 1: the helper keeps connections open, so nc needs an idle timeout to exit
reply="$(printf 'ping\n' | nc -w 1 -U "$SOCKET" || true)"
if [ "$reply" != "ok" ]; then
    echo "unexpected reply from helper: '$reply'; see /var/log/awake-helper.log" >&2
    exit 1
fi
echo "helper installed and responding for uid $UID_NUM"
# ---- end helper ----
