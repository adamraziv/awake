# Awake: design spec

Date: 2026-10-16
Status: approved in brainstorming, pending written review

## 1. Purpose

A lightweight macOS menu bar app that keeps the Mac awake on demand, the way Amphetamine promises to but with the failure modes fixed. It must:

- Prevent system sleep while a session is active, including when the lid is closed and no external display is attached.
- Allow the display to turn off on its normal schedule during a session.
- Keep the network up during a session so SSH, Tailscale, and downloads survive.
- Offer a remote control page, reachable only over the user's Tailscale network, that controls the app (not the Mac's desktop) and shows system metrics.
- Use very little memory. Target: under 15 MB resident for the app, under 3 MB for the helper.

Non-goals for v1: custom timer entry, app-triggered sessions ("keep awake while X is running"), scheduling, network throughput metrics, per-process views, GPU or fan readings, a browser extension, multi-user support, distribution to other people (no notarization, no installer package).

## 2. Why Amphetamine fails, and the fix

A normal IOKit sleep assertion (`PreventUserIdleSystemSleep`) blocks idle sleep only. Lid-close sleep on a laptop without an external display is enforced by the kernel regardless of assertions. The only override is the power management setting `disablesleep`, which requires root. Network drops come from the Mac entering low-power idle states that a plain assertion does not cover.

The fix is a session that does both: hold the idle-sleep assertion and flip `disablesleep` via a privileged helper. Because `disablesleep` left on after a crash would stop the Mac from ever sleeping, the helper is designed so that it resets the setting itself whenever the app goes away.

## 3. Architecture

Three Rust crates in one Cargo workspace. No Swift or Objective-C source.

### 3.1 `awake-app` (menu bar app, runs as the logged-in user)

Owns everything user-facing and everything that does not need root:

- Status item and menu (AppKit via the `objc2` crate family: `objc2-app-kit`, `objc2-foundation`).
- Session state machine and timer.
- Idle-sleep assertion via IOKit FFI (`IOPMAssertionCreateWithName` / `IOPMAssertionRelease`).
- Screen off and lock actions.
- Battery guard.
- Metrics collector.
- HTTP server for the remote page, bound to the Tailscale interface only.
- Tailscale detection and identity lookup.
- Client connection to the helper socket.

Threading: the AppKit main loop runs on the main thread. A single Tokio runtime on a second thread runs the HTTP server, the timer, the metrics tick, the Tailscale re-detect loop, and the helper client. State shared between them lives in one `Arc<Mutex<AppState>>`; the menu is refreshed by posting to the main thread.

Packaged as a minimal `.app` bundle with `LSUIElement = true` so it has a menu bar presence, no Dock icon, and can be a login item. Launch at login is registered with `SMAppService.mainApp` (via `objc2-service-management`) and exposed as a menu toggle, default on.

### 3.2 `awake-helper` (root daemon, launchd)

Installed to `/Library/PrivilegedHelperTools/com.voltron.awake-helper` with a launchd plist at `/Library/LaunchDaemons/com.voltron.awake-helper.plist` (`RunAtLoad`, `KeepAlive`). Listens on a Unix socket at `/var/run/awake-helper.sock`, mode 0666. File permissions are not the access control; the peer-credential check below is.

Every accepted connection is checked with `getpeereid`. Connections from any UID other than the one baked in at install time (written into the plist as an environment variable `AWAKE_ALLOWED_UID`) are closed immediately.

Commands:

| Command | Action |
|---|---|
| `sleep_disable` | `pmset -a disablesleep 1`; marks this connection as the owner |
| `sleep_enable` | `pmset -a disablesleep 0`; clears owner |
| `shutdown` | `pmset -a disablesleep 0` then `shutdown -h now` |
| `ping` | replies `ok` (used for the connected indicator) |

Fail-safes in the helper:

- When the owner connection closes for any reason, run `sleep_enable`.
- On helper start, run `sleep_enable`.
- Only one owner at a time. A second `sleep_disable` from another connection is refused with `busy`.

The helper runs `pmset` by absolute path `/usr/bin/pmset` and `shutdown` by `/sbin/shutdown`, never via a shell.

### 3.3 `awake-proto` (shared crate)

The message format on the helper socket. Newline-delimited UTF-8 text. Requests are a single word from the command table. Responses are `ok`, `busy`, or `err <message>`. The crate exposes `encode_request`, `parse_request`, `encode_response`, `parse_response` and nothing else. Both binaries depend on it so neither hand-rolls parsing.

### 3.4 Install and uninstall

`scripts/install.sh`:

1. `cargo build --release` for the workspace.
2. Assemble `Awake.app` from `packaging/Info.plist` and the `awake-app` binary into `~/Applications/Awake.app`. Ad-hoc codesign it.
3. With `sudo`: copy the helper binary, write the launchd plist with the current UID, `launchctl bootstrap system`.
4. Open the app.

`scripts/uninstall.sh` reverses step 3 and removes the bundle. It runs `pmset -a disablesleep 0` before unloading the helper.

This is the only place `sudo` appears. The app itself never escalates.

## 4. Session semantics

The app is in exactly one of two states: `Idle` or `Active { ends_at: Option<Instant> }`.

### 4.1 Start

In order:

1. Create the `PreventUserIdleSystemSleep` assertion with a descriptive name ("Awake session").
2. Send `sleep_disable` to the helper. If the helper is not connected, continue, but set `lid_protection = false` and show a warning in the menu and the status endpoint: "Lid-close protection unavailable, helper not running."
3. Record `ends_at` if a duration was given.
4. Update the menu bar icon to the active glyph.

A start while already active replaces the timer (so "start 1h" during an indefinite session gives it a 1h end). It does not re-create the assertion.

### 4.2 End

Exactly one function, `end_session`, called from every path: menu click, timer expiry, remote stop, remote shutdown, battery guard, app quit. In order:

1. Send `sleep_enable` to the helper (ignore errors; the helper's disconnect fail-safe covers it).
2. Release the assertion.
3. Set state to `Idle`, update the icon.

### 4.3 Timer presets

30 min, 1 h, 2 h, 4 h, 8 h, and "Until I stop it". The menu shows remaining time while active, refreshed once a minute.

### 4.4 Battery guard

Default on. When the battery is at or below 10% and the Mac is not on AC, end the session and show a menu note "Session ended: battery guard". Toggle in the menu, persisted in `~/Library/Application Support/awake/config.json` along with the launch-at-login preference. Threshold is fixed at 10% in v1.

### 4.5 App-level fail-safes

- On app start, send `sleep_enable` once, in case the previous shutdown left the setting on and the helper reset did not run.
- The helper connection is kept open for the app's lifetime and reconnected with backoff (1s, 2s, 4s, max 30s) if it drops. The menu shows "Helper: connected" or "Helper: not running".
- If the connection drops during an active session, the helper has already reset sleep. When the app reconnects, it re-sends `sleep_disable` so the session's lid protection resumes.

## 5. Local actions

| Action | Mechanism | Needs root |
|---|---|---|
| Screen off | `/usr/bin/pmset displaysleepnow`, run directly without a shell | No |
| Lock | `SACLockScreenImmediate` from the private `login` framework, with fallback to `/System/Library/CoreServices/Menu Extras/User.menu/Contents/Resources/CGSession -suspend` | No |
| Shut down | helper `shutdown` command | Yes |

Screen off does not end the session. Lock does not end the session. Shut down ends the session first.

## 6. Remote control

### 6.1 Binding

On start and every 30 seconds the app enumerates network interfaces (`getifaddrs`) looking for an IPv4 address in 100.64.0.0/10. If found and the server is not running, bind to `<that ip>:7700`. If the address disappears, stop the server. The server never binds to localhost, LAN addresses, or wildcard. The menu shows the current URL or "Remote: off (no Tailscale)".

### 6.2 Auth

For each request, the app resolves the peer IP through Tailscale's identity lookup and compares the resulting login name to this node's own login name. Only a match is allowed; everything else is 403.

Lookup mechanism: run the `tailscale` CLI that ships inside the Tailscale app bundle (`/Applications/Tailscale.app/Contents/MacOS/Tailscale`) with `whois --json <ip>` and `status --json` (for the self login name). Results are cached per IP for 60 seconds. The self login name is cached for 5 minutes. Any CLI failure, parse failure, or missing CLI yields 403, never a fallthrough. The CLI path is configurable in `config.json` because the App Store and standalone Tailscale builds install to different places.

Tailscale was not installed on the target Mac at design time. The design must work with it absent: the server simply does not start.

### 6.3 API

JSON over HTTP/1.1. The HTTP stack is `axum` on `tokio` with minimal features. No websockets; the page polls.

| Method and path | Request | Response |
|---|---|---|
| `GET /` | | the embedded page, `text/html` |
| `GET /api/status` | | see below |
| `POST /api/session/start` | `{ "minutes": 60 }` or `{ "minutes": null }` | status |
| `POST /api/session/stop` | | status |
| `POST /api/screen/off` | | `{ "ok": true }` |
| `POST /api/lock` | | `{ "ok": true }` |
| `POST /api/shutdown` | `{ "confirm": true }` | `{ "ok": true }`, 400 if `confirm` is not `true` |

Status shape:

```json
{
  "session": { "active": true, "ends_at": "2026-10-16T12:00:00Z", "remaining_seconds": 3540, "lid_protection": true },
  "helper_connected": true,
  "battery_guard": true,
  "metrics": {
    "cpu_percent": 12.5,
    "cpu_cores": 10,
    "ram_used_bytes": 1234, "ram_total_bytes": 5678, "memory_pressure": "normal",
    "disk_used_bytes": 1234, "disk_total_bytes": 5678,
    "battery_percent": 87, "charging": false, "on_ac": true,
    "uptime_seconds": 12345,
    "cpu_temp_c": 48.2,
    "thermal_state": "nominal",
    "sampled_at": "2026-10-16T11:01:00Z"
  }
}
```

`cpu_temp_c` is `null` when unavailable. `thermal_state` is one of `nominal`, `fair`, `serious`, `critical`.

### 6.4 Page

One HTML file at `crates/awake-app/web/index.html`, embedded with `include_str!`. Vanilla HTML, CSS, and JavaScript, no framework, no build step. Mobile-first layout, dark and light via `prefers-color-scheme`.

Layout top to bottom:

1. Status card: Active or Idle, remaining time, lid protection indicator, battery percent.
2. Session controls: one button per timer preset plus "Until I stop it", and a Stop button shown only when active.
3. Quick actions: Screen off, Lock.
4. Metrics tiles: CPU, RAM, Disk, Battery, Temp or Thermal, Uptime.
5. Danger zone, visually separated: Shut down. First tap reveals a confirm button; second tap sends the request.

Polls `GET /api/status` every 5 seconds. A failed poll shows a "Disconnected" banner and keeps retrying.

## 7. Metrics

Collected on a 5-second tick by one task, stored in `AppState`, served from the cache. No shelling out, no third-party monitoring crate.

| Metric | Source |
|---|---|
| CPU percent | Mach `host_processor_info`, delta of ticks between samples |
| RAM | Mach `host_statistics64` (`vm_statistics64`) and `sysctl hw.memsize`; memory pressure from `sysctl kern.memorystatus_vm_pressure_level` |
| Disk | `statfs("/")` |
| Battery | IOKit `IOPSCopyPowerSourcesInfo` |
| Uptime | `sysctl kern.boottime` |
| CPU temperature | IOHIDEventSystem sensor path (undocumented, as used by macmon). Average of CPU die sensors. Best-effort. |
| Thermal state | `sysctl` thermal level, mapped to the four states |

The temperature reader is isolated behind a trait and may return `None` without affecting anything else.

## 8. Error handling

- No subsystem failure crashes the app. Helper, Tailscale, HTTP server, metrics tick, and temperature reader each fail independently and surface a status line in the menu and in `/api/status`.
- App logs to `~/Library/Logs/awake/awake.log` with daily rotation, 7 files kept. Menu item "Reveal log".
- Helper logs via launchd stdout/stderr to `/var/log/awake-helper.log`.
- The helper never panics on bad input: unknown commands get `err unknown command`, oversized lines (over 256 bytes) close the connection.

## 9. Testing

Automated:

- `awake-proto`: encode and decode round-trips, malformed input, oversized input.
- Session state machine: pure Rust behind a `SleepController` trait with a fake. Cases: start, stop, timer expiry, battery guard trips, start while active replaces the timer, stop while idle is a no-op, cleanup ordering (helper before assertion on end), helper unavailable sets `lid_protection = false`.
- Helper: command handler with a fake command runner; integration test spawning the helper unprivileged on a temp socket, verifying UID rejection and that owner disconnect triggers `sleep_enable`.
- HTTP: router tests with a fake identity resolver and fake session. Cases: 403 on identity mismatch, 403 on resolver failure, 400 on shutdown without confirm, status shape, start with and without minutes.
- Metrics: smoke tests asserting plausible ranges on the host; temperature test accepts `None`.

Manual checklist, run before calling v1 done:

1. Start an indefinite session, close the lid, SSH in from another machine over 10 minutes. Connection stays up.
2. Start a 30-minute session, wait. Sleep setting returns to normal at expiry (`pmset -g | grep SleepDisabled` shows 0).
3. Force-quit the app during a session. Within 5 seconds `SleepDisabled` is 0.
4. Reboot with the helper installed. `SleepDisabled` is 0 at login.
5. Open the page from a phone on the tailnet. Controls work.
6. Request from a tailnet node logged in as a different user gets 403.
7. Resident memory of the app under 15 MB after one hour (Activity Monitor).

## 10. Repository layout

```
awake/
  Cargo.toml                       # workspace
  crates/
    awake-proto/src/lib.rs
    awake-helper/src/main.rs
    awake-app/
      src/
        main.rs                    # AppKit bootstrap, runtime spawn
        menu.rs                    # status item, menu items, icon
        session.rs                 # state machine, timer, battery guard
        sleep.rs                   # IOKit assertion, SleepController trait, helper client
        power.rs                   # screen off, lock
        metrics/{mod,cpu,mem,disk,battery,thermal}.rs
        remote/{mod,server,auth,status}.rs
        tailscale.rs               # interface detection, CLI identity lookup
        config.rs
      web/index.html
  packaging/Info.plist
  packaging/com.voltron.awake-helper.plist
  scripts/{install.sh,uninstall.sh,build-app.sh}
  docs/superpowers/specs/
```

## 11. Build order

1. `awake-proto` and `awake-helper`, with the install script. Verify lid-close works with the helper alone via a manual socket client.
2. `awake-app` core: menu, session, sleep, power, config, launch at login. Shippable on its own at this point.
3. Metrics collector.
4. Remote: Tailscale detection, auth, server, page.
