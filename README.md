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

- **Status icon** driven by overall health:

  | State | Icon | Meaning |
  |---|---|---|
  | Stopped | dimmed mark, no emblem | unit not installed, or not running |
  | Starting | blue dot | unit is up, REST API not answering yet |
  | Syncing | blue dot | at least one folder syncing or scanning |
  | Error | red exclamation | unit failed, or a folder has errors |
  | Paused | amber pause bars | every folder is paused |
  | OK | plain mark, no emblem | everything up to date |

  The artwork is the Syncthing mark - the ring with three outer nodes linked to
  a central one - traced from the project's own `logo-only.svg` into
  `resources/icons/syncthing.svg`, then recoloured per state and badged with a
  small emblem. State is carried by emblem *shape* as well as colour, so the
  states stay distinguishable without relying on hue.

  Two styles are available, chosen with `SYNCERTING_ICON_STYLE`:

  | Value | Result |
  |---|---|
  | unset, or `mono` | the mark alone, recoloured by the panel (default) |
  | `colour` | the mark on its blue disc, in full colour |

  Monochrome is the default because Plasma styles its tray icons symbolically,
  and a colour icon looks foreign beside the rest of the panel.

  The two styles reach the panel by different routes. The monochrome icons are
  written to `~/.cache/syncerting-tray/icons` as a small `hicolor` theme and
  referenced *by name*, because recolouring to match the panel happens inside
  Plasma's icon engine and applies only to icons it loads from a theme — a
  pixmap would be stuck with whatever colour was baked into it. It has to be
  `hicolor` specifically, since that is the fallback every theme inherits. The
  colour icons are the opposite: embedded in the binary and sent as pixmaps,
  since no icon theme ships a Syncthing status icon and a named icon would risk
  an empty tray slot.

  There is no autodetection: the StatusNotifierItem protocol never tells an item
  how the panel is styled, so the style is a setting rather than something
  observed.

- **Left click** opens the menu. Everything the tray does lives there, so a
  click that did something else would be a shortcut with no way to discover it.
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
- The Breeze icon theme, for the menu icons
- `syncthing` on `PATH` (see below)

## Install

From crates.io:

```sh
cargo install syncerting-tray
```

Or from a checkout:

```sh
just install    # builds --release, then installs the binary and autostart entry
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

Use the **Start Tray at Login** item in the menu; it writes the autostart entry
for you. The file can also be installed by hand:

```sh
install -Dm644 resources/syncerting-tray.desktop \
  ~/.config/autostart/syncerting-tray.desktop
```

Note this autostarts the *tray*. Syncthing itself is autostarted by its own
systemd unit, via the "Start Syncthing at Login" toggle.

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
| `SYNCERTING_ICON_STYLE` | `colour` for the full-colour icons; monochrome is the default |

A wildcard GUI bind address such as `0.0.0.0:8384` is rewritten to loopback, since
a bind address is not necessarily connectable. Syncthing's GUI certificate is
self-signed, so when the GUI has TLS enabled, certificate verification is waived —
the connection is to loopback, where there is nothing meaningful to verify against.

## How it stays current

A long poll against `/rest/events` wakes the tray as soon as anything changes.
Bursts of events are coalesced into at most one refresh per second, and a full
refresh runs every 10 seconds regardless, so the display self-heals if an event
is ever missed.

## Releasing

`cargo publish` runs from CI, not from a workstation. Pushing a tag triggers
`.github/workflows/release.yml`, which gates on clippy and the test suite before
publishing.

Authentication is by OIDC trusted publishing via `rust-lang/crates-io-auth-action`,
so no registry token is stored in the repository; the `release` environment on
GitHub is what authorises it. Set the crate up for trusted publishing on
crates.io once, naming this repository and the `release` environment.

```sh
just package                 # verify the crate as crates.io would receive it
git tag v0.1.1 && git push --tags
```

`cargo package` is worth running before tagging: it builds from the generated
tarball rather than the working tree, which is what catches a file the crate
needs but does not ship. The tray reads its artwork with `include_str!`, so
`resources/` is listed in `include` for exactly that reason.

## Licence

Apache-2.0. See [LICENSE](LICENSE).
