//! The StatusNotifierItem presented to Plasma.
//!
//! Menu callbacks are synchronous and run on the tray's own task, so they only
//! post a [`Command`] to the worker rather than doing any I/O themselves.

use crate::icons;
use crate::model::{ApiState, AppState, Command};
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{MenuItem, ToolTip};
use tokio::sync::mpsc::UnboundedSender;

pub struct SyncthingTray {
    pub state: AppState,
    /// Fixed at startup; the panel never tells us its styling, so this is a
    /// setting rather than something observed.
    style: icons::Style,
    /// Root of the generated monochrome icon theme, empty in the colour style.
    theme_path: String,
    tx: UnboundedSender<Command>,
}

impl SyncthingTray {
    pub fn new(tx: UnboundedSender<Command>, style: icons::Style, theme_path: String) -> Self {
        Self {
            state: AppState::default(),
            style,
            theme_path,
            tx,
        }
    }

    fn sender(&self) -> UnboundedSender<Command> {
        self.tx.clone()
    }

    /// A menu entry that posts `command` when clicked.
    fn action(&self, label: &str, icon: &str, enabled: bool, command: Command) -> MenuItem<Self> {
        let tx = self.sender();
        StandardItem {
            label: label.into(),
            icon_name: icon.into(),
            enabled,
            activate: Box::new(move |_: &mut Self| {
                let _ = tx.send(command.clone());
            }),
            ..Default::default()
        }
        .into()
    }

    /// Non-interactive text row.
    fn label(text: String) -> MenuItem<Self> {
        StandardItem {
            label: text,
            enabled: false,
            ..Default::default()
        }
        .into()
    }

    /// One row per folder, showing its state. Clicking opens the folder.
    ///
    /// Flat rather than a submenu per folder: submenus are populated lazily by
    /// the host and ksni does not answer `AboutToShow` for anything but the root
    /// item, so a submenu arrives empty. Per-folder rescan and pause live in the
    /// web UI, which this tray defers to for anything deeper anyway.
    fn folder_items(&self) -> Vec<MenuItem<Self>> {
        if self.state.api != ApiState::Connected {
            return Vec::new();
        }
        if self.state.folders.is_empty() {
            return vec![Self::label("No folders configured".into())];
        }

        self.state
            .folders
            .iter()
            .map(|folder| {
                let tx_open = self.sender();
                let path = folder.path.clone();

                StandardItem {
                    label: format!("{}  \u{2014}  {}", folder.display_name(), folder.summary()),
                    icon_name: if folder.errors > 0 {
                        "state-error".into()
                    } else if folder.paused {
                        "state-pause".into()
                    } else if folder.state == "syncing" || folder.state == "scanning" {
                        "state-sync".into()
                    } else {
                        "state-ok".into()
                    },
                    enabled: !folder.path.is_empty(),
                    activate: Box::new(move |_: &mut Self| {
                        let _ = tx_open.send(Command::OpenFolder(path.clone()));
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect()
    }

    /// Service controls, appended inline for the same reason as the folders.
    fn service_items(&self) -> Vec<MenuItem<Self>> {
        let unit = &self.state.unit;
        let busy = self.state.busy;

        if !unit.installed {
            return vec![
                Self::label("syncthing.service is not installed".into()),
                self.action(
                    "Install User Service\u{2026}",
                    "document-save",
                    !busy,
                    Command::InstallUnit,
                ),
            ];
        }

        let tx_enable = self.sender();
        let enabled_now = unit.is_enabled();

        vec![
            self.action(
                "Start Syncthing",
                "media-playback-start",
                !busy && !unit.is_active(),
                Command::Start,
            ),
            self.action(
                "Stop Syncthing",
                "media-playback-stop",
                !busy && unit.is_active(),
                Command::Stop,
            ),
            self.action("Restart Syncthing", "view-refresh", !busy, Command::Restart),
            CheckmarkItem {
                label: "Start Syncthing at Login".into(),
                checked: enabled_now,
                enabled: !busy,
                activate: Box::new(move |_: &mut Self| {
                    let _ = tx_enable.send(Command::SetEnabled(!enabled_now));
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

impl ksni::Tray for SyncthingTray {
    /// Left click opens the menu rather than firing an action. Everything the
    /// tray does lives in the menu, so a click that did something else would be
    /// a hidden shortcut with no way to discover it.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn title(&self) -> String {
        "Syncthing".into()
    }

    /// Empty in the colour style, so the host uses the pixmaps below. In the
    /// monochrome style the icon is named instead, which is what lets Plasma
    /// repaint it to match the panel. A Breeze name is the last resort, so the
    /// tray slot is never blank.
    fn icon_name(&self) -> String {
        let health = self.state.health();
        match self.style {
            icons::Style::Monochrome => icons::icon_name(health),
            icons::Style::Colour if icons::available(health, self.style) => String::new(),
            icons::Style::Colour => health.icon_name().into(),
        }
    }

    /// Search path for the generated monochrome theme; ignored by the host when
    /// empty, which is the case in the colour style.
    fn icon_theme_path(&self) -> String {
        self.theme_path.clone()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        icons::for_health(self.state.health(), self.style)
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::SystemServices
    }

    fn tool_tip(&self) -> ToolTip {
        let mut description = self.state.headline();
        if let Some(version) = &self.state.version {
            description.push_str(&format!("\nSyncthing {version}"));
        }
        if let Some(error) = &self.state.last_error {
            description.push_str(&format!("\n{error}"));
        }

        ToolTip {
            icon_pixmap: icons::for_health(self.state.health(), self.style),
            title: "Syncthing".into(),
            description,
            ..Default::default()
        }
    }

    // `menu_about_to_show` is deliberately not implemented. Overriding it makes
    // ksni rebuild the layout at the moment the menu opens, and a layout change
    // while the menu is on screen reassigns item ids underneath the host, which
    // stops submenus opening. ksni carries a FIXME about exactly this case.
    //
    // Nothing is lost by leaving it out: the worker refreshes on every event and
    // at least every ten seconds, so the menu is already current when it opens.

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let connected = self.state.api == ApiState::Connected;
        let mut items = vec![Self::label(self.state.headline())];

        if let ApiState::NotConfigured(why) = &self.state.api {
            items.push(Self::label(format!("  {why}")));
        }
        if let Some(error) = &self.state.last_error {
            items.push(Self::label(format!("  {error}")));
        }

        items.push(MenuItem::Separator);
        items.extend(self.folder_items());

        if connected {
            items.push(MenuItem::Separator);
        }

        items.push(self.action(
            "Open Web UI",
            "internet-web-browser",
            true,
            Command::OpenWebUi,
        ));
        items.push(self.action("Rescan All", "view-refresh", connected, Command::RescanAll));

        let all_paused = self.state.all_paused();
        items.push(self.action(
            if all_paused {
                "Resume All"
            } else {
                "Pause All"
            },
            if all_paused {
                "media-playback-start"
            } else {
                "media-playback-pause"
            },
            connected,
            Command::SetAllPaused(!all_paused),
        ));

        items.push(MenuItem::Separator);
        items.extend(self.service_items());
        items.push(MenuItem::Separator);

        let tx_autostart = self.sender();
        let autostart_now = self.state.autostart;
        items.push(
            CheckmarkItem {
                label: "Start Tray at Login".into(),
                checked: autostart_now,
                enabled: !self.state.busy,
                activate: Box::new(move |_: &mut Self| {
                    let _ = tx_autostart.send(Command::SetAutostart(!autostart_now));
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(self.action("Quit", "application-exit", true, Command::Quit));

        items
    }
}
