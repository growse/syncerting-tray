# syncerting-tray

A Syncthing tray application for KDE Plasma.

It shows Syncthing's sync state in the system tray, controls the `syncthing.service`
systemd user unit, and hands off anything deeper to Syncthing's own web UI.

## Why there is no Qt here

On Plasma the system tray is not an X11 icon — it is the
[StatusNotifierItem](https://www.freedesktop.org/wiki/Specifications/StatusNotifierItem/)
D-Bus specification. `QSystemTrayIcon` is simply one client of that spec. This talks
the protocol directly via [`ksni`](https://crates.io/crates/ksni), so Plasma renders
the icon and menu natively, with no Qt dependency and no toolkit theming mismatch.

## What it does

- **Status icon** driven by overall health, using Breeze's `state-*` icons:

  | State | Icon | Meaning |
  |---|---|---|
  | Stopped | `state-offline` | unit not installed, or not running |
  | Starting | `state-sync` | unit is up, REST API not answering yet |
  | Syncing | `state-sync` | at least one folder syncing or scanning |
  | Error | `state-error` | unit failed, or a folder has errors |
  | Paused | `state-pause` | every folder is paused |
  | OK | `state-ok` | everything up to date |

- **Per-folder submenus** with completion percentage, plus rescan, open folder,
  and pause/resume.
- **Service control** — start, stop, restart, and a "Start at Login" toggle that
  enables the unit.
- **Left click** opens the web UI.
- **Error dialogs** for failed actions. A tray menu closes the instant it is
  clicked, so an error recorded only in the menu would be invisible until the menu
  is reopened. Anything you explicitly asked for therefore also reports failure in
  a dialog, via `kdialog`, falling back to `zenity` then `notify-send`. Background
  refresh failures are *not* shown this way — a Syncthing that is merely down would
  otherwise produce a dialog every few seconds.
- **Single instance.** Ownership of the `dev.growse.SyncertingTray` bus name acts
  as the lock, so a second launch exits quietly rather than adding a duplicate
  icon. The bus releases the name automatically when the process dies, so there is
  no lock file to go stale after a crash.

## Requirements

- KDE Plasma, or any desktop running a `org.kde.StatusNotifierWatcher`
- A systemd user session
- The Breeze icon theme, for the `state-*` icons
- `syncthing` on `PATH` (see below)

## Build

```sh
cargo build --release
install -Dm755 target/release/syncerting-tray ~/.local/bin/syncerting-tray
```

## First run

Syncthing itself is **not** installed by this app. Install it with your package
manager first:

```sh
sudo pacman -S syncthing      # Arch
sudo apt install syncthing    # Debian/Ubuntu
```

Most distributions package the user unit already — Arch, for example, ships
`/usr/lib/systemd/user/syncthing.service`. Where that is the case there is nothing
to install, and **Service → Start at Login** is all you need.

Only if no `syncthing.service` unit exists at all does the menu offer
**Service → Install User Service…**, which writes one to
`~/.config/systemd/user/`, enables it, and starts it. Note that a unit written
there shadows a packaged one, which is why it is offered only when nothing else
provides the unit.

Until Syncthing has run once and written its `config.xml`, the menu says so
explicitly rather than failing silently.

## Autostart

To start the tray on login:

```sh
install -Dm644 resources/syncerting-tray.desktop \
  ~/.config/autostart/syncerting-tray.desktop
```

Note this autostarts the *tray*. Syncthing itself is autostarted by its own
systemd unit, via the "Start at Login" toggle.

## Configuration

Connection details are read from Syncthing's own `config.xml`, searched in order:

1. `$STHOMEDIR/config.xml`
2. `$XDG_STATE_HOME/syncthing/config.xml` (the default since Syncthing 1.27)
3. `$XDG_CONFIG_HOME/syncthing/config.xml`

Environment overrides:

| Variable | Effect |
|---|---|
| `SYNCTHING_API_KEY` | Override the API key from `config.xml` |
| `SYNCTHING_URL` | Point at a different instance, e.g. `http://127.0.0.1:8385` |
| `SYNCTHING_BINARY` | Absolute path used for `ExecStart` when writing the unit |

A wildcard GUI bind address such as `0.0.0.0:8384` is rewritten to loopback, since
a bind address is not necessarily connectable. Syncthing's GUI certificate is
self-signed, so when the GUI has TLS enabled, certificate verification is waived —
the connection is to loopback, where there is nothing meaningful to verify against.

## How it stays current

A long poll against `/rest/events` wakes the tray as soon as anything changes.
Bursts of events are coalesced into at most one refresh per second, and a full
refresh runs every 10 seconds regardless, so the display self-heals if an event
is ever missed.
