# Awake

A tiny macOS menu bar app that keeps your Mac awake, including with the lid closed, and lets you control it from your phone over Tailscale.

It exists because Amphetamine-style apps rely on power assertions alone, which macOS ignores when you close the lid without an external display. Awake pairs the assertion with `pmset disablesleep`, run by a small root helper, so the Mac stays awake with the lid shut and SSH, Tailscale, and downloads keep working. The screen still turns off.

Written in Rust (no Swift or Objective-C). The app uses about 11 MB of memory.

## Features

- **Menu bar eye:** closed when idle, open while a session runs.
- **Sessions:** start indefinitely or with a timer (30 min, 1, 2, 4, or 8 hours).
- **Lid closed is fine:** the display goes dark, the Mac keeps running.
- **Battery guard:** ends the session at 10% on battery (toggle in the menu).
- **Quick actions:** Screen off and Lock.
- **Remote page:** a phone-friendly web page served only on your Tailscale address. Start and stop sessions, turn the screen off, lock, shut down, and watch CPU, RAM, disk, battery, CPU temperature, and uptime.
- **Fail-safe:** if the app quits or crashes, the helper restores normal sleep within seconds.

## Requirements

- Apple Silicon Mac, macOS 14 or later (developed on macOS 26).
- A recent stable Rust toolchain, installed with `rustup` (developed with Rust 1.99).
- For remote control: [Tailscale](https://tailscale.com/download/mac) on the Mac and on your phone, signed in to the same account. The standalone Tailscale app is expected at `/Applications/Tailscale.app`.

## Install

```bash
git clone git@github.com:adamraziv/awake.git
cd awake
./scripts/install.sh
```

The script builds everything, installs the root helper as a launchd daemon (it asks for your password once), puts `Awake.app` in `~/Applications`, and launches it. Awake registers itself to open at login; you can turn that off in its menu.

To update later, pull and run `./scripts/install.sh` again. If only the app changed, `./scripts/build-app.sh` rebuilds the app without touching the helper; quit Awake and reopen it afterwards.

## Use

Click the eye in the menu bar:

- **Start session** / **Start with timer**: keep the Mac awake.
- **Stop session**: back to normal sleep.
- **Screen off** / **Lock**: these do not end the session.
- **Battery guard**, **Launch at login**: toggles.
- **Helper** and **Remote** lines show whether the helper is running and the remote page address.

## Connect from your phone

1. Make sure Tailscale is running on the Mac and your phone, signed in to the same account. Turning on Tailscale's "launch at login" (or adding it under System Settings, General, Login Items) keeps the page available after a restart.
2. Open the Awake menu. The **Remote** line shows the address, for example `http://100.x.y.z:7700`.
3. Open that address on your phone. Bookmark it or add it to the home screen. Your Mac's MagicDNS name works too: `http://<mac-name>:7700`.

Who can connect:

- The page listens only on the Mac's Tailscale address, never on localhost or your local network.
- Each request is checked with `tailscale whois`: only devices signed in to the same Tailscale account as the Mac get in. Everyone else gets 403.
- There are no passwords or tokens. The connection is plain HTTP, but Tailscale encrypts it end to end.

If Tailscale goes offline, the server stops. It comes back within 30 seconds of Tailscale reconnecting.

Shut down on the remote page asks for a second tap. A Mac that is shut down can't be turned on remotely, and with FileVault on it waits for your password after power-on.

## Configuration

`~/Library/Application Support/awake/config.json` (created when you change a setting):

```json
{
  "battery_guard": true,
  "launch_at_login": true,
  "tailscale_cli": "/Applications/Tailscale.app/Contents/MacOS/Tailscale"
}
```

Set `tailscale_cli` if your Tailscale CLI lives elsewhere, for example the App Store build.

## Uninstall

```bash
./scripts/uninstall.sh
```

This quits and removes the app, removes the helper, and resets the sleep setting. If Awake is still listed under System Settings, General, Login Items, remove it there.

## Troubleshooting

- **Logs:** Reveal log in the menu, or `~/Library/Logs/awake/`. The helper logs to `/var/log/awake-helper.log`.
- **"Helper: not running":** run `./scripts/install.sh` again. Without the helper, sessions still prevent idle sleep but closing the lid will sleep the Mac.
- **"Remote: off (no Tailscale)":** Tailscale is not connected on the Mac.
- **Check the sleep state:** `pmset -g | grep SleepDisabled` reads 1 during a session and 0 otherwise.

## How it works

Three crates in one Cargo workspace:

- `awake-app`: the menu bar app (AppKit through `objc2`), the session logic, the metrics, and the remote server (`axum` on a small Tokio runtime). The web page is one HTML file compiled into the binary.
- `awake-helper`: the root daemon. It only toggles `pmset disablesleep` and runs shutdown, and it accepts commands only from your user over a Unix socket. If the app disconnects, it restores normal sleep.
- `awake-proto`: the message format they share.

## Development

```bash
cargo test                         # all crates
cargo clippy --workspace --all-targets
cargo run -p awake-app             # run unbundled (don't do this while the installed app holds a session)
```

## License

MIT
