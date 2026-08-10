//! The StatusNotifierItem presented to Plasma.
//!
//! Menu callbacks are synchronous and run on the tray's own task, so they only
//! post a [`Command`] to the worker rather than doing any I/O themselves.

use crate::icons;
use crate::model::{ApiState, AppState, Command};
use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
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

    /// The per-folder section, or an explanatory row when there is nothing to show.
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
                let tx_scan = self.sender();
                let tx_open = self.sender();
                let tx_pause = self.sender();

                let id_scan = folder.id.clone();
                let id_pause = folder.id.clone();
                let path = folder.path.clone();
                let paused = folder.paused;
                let has_path = !folder.path.is_empty();

                SubMenu {
                    label: format!("{}  —  {}", folder.display_name(), folder.summary()),
                    icon_name: if folder.errors > 0 {
                        "state-error".into()
                    } else if folder.paused {
                        "state-pause".into()
                    } else if folder.state == "syncing" || folder.state == "scanning" {
                        "state-sync".into()
                    } else {
                        "state-ok".into()
                    },
                    submenu: vec![
                        StandardItem {
                            label: "Rescan".into(),
                            icon_name: "view-refresh".into(),
                            enabled: !paused,
                            activate: Box::new(move |_: &mut Self| {
                                let _ = tx_scan.send(Command::RescanFolder(id_scan.clone()));
                            }),
                            ..Default::default()
                        }
                        .into(),
                        StandardItem {
                            label: "Open Folder".into(),
                            icon_name: "folder-open".into(),
                            enabled: has_path,
                            activate: Box::new(move |_: &mut Self| {
                                let _ = tx_open.send(Command::OpenFolder(path.clone()));
                            }),
                            ..Default::default()
                        }
                        .into(),
                        MenuItem::Separator,
                        StandardItem {
                            label: if paused {
                                "Resume".into()
                            } else {
                                "Pause".into()
                            },
                            icon_name: if paused {
                                "media-playback-start".into()
                            } else {
                                "media-playback-pause".into()
                            },
                            activate: Box::new(move |_: &mut Self| {
                                let _ = tx_pause
                                    .send(Command::SetFolderPaused(id_pause.clone(), !paused));
                            }),
                            ..Default::default()
                        }
                        .into(),
                    ],
                    ..Default::default()
                }
                .into()
            })
            .collect()
    }

    /// Start/stop/restart plus the enable-at-login toggle.
    fn service_menu(&self) -> MenuItem<Self> {
        let unit = &self.state.unit;
        let busy = self.state.busy;

        if !unit.installed {
            return SubMenu {
                label: "Service".into(),
                icon_name: "system-run".into(),
                submenu: vec![
                    Self::label("syncthing.service is not installed".into()),
                    self.action(
                        "Install User Service…",
                        "document-save",
                        !busy,
                        Command::InstallUnit,
                    ),
                ],
                ..Default::default()
            }
            .into();
        }

        let tx_enable = self.sender();
        let enabled_now = unit.is_enabled();

        SubMenu {
            label: "Service".into(),
            icon_name: "system-run".into(),
            submenu: vec![
                self.action(
                    "Start",
                    "media-playback-start",
                    !busy && !unit.is_active(),
                    Command::Start,
                ),
                self.action(
                    "Stop",
                    "media-playback-stop",
                    !busy && unit.is_active(),
                    Command::Stop,
                ),
                self.action("Restart", "view-refresh", !busy, Command::Restart),
                MenuItem::Separator,
                CheckmarkItem {
                    label: "Start at Login".into(),
                    checked: enabled_now,
                    enabled: !busy,
                    activate: Box::new(move |_: &mut Self| {
                        let _ = tx_enable.send(Command::SetEnabled(!enabled_now));
                    }),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                Self::label(format!("Unit state: {}", unit.active_state)),
            ],
            ..Default::default()
        }
        .into()
    }
}

impl ksni::Tray for SyncthingTray {
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

    /// Left click opens the web UI, which is the most common thing to want.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(Command::OpenWebUi);
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

    /// Refresh on menu open so the contents are current even if an event was missed.
    fn menu_about_to_show(&mut self) {
        let _ = self.tx.send(Command::Refresh);
    }

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
        items.push(self.service_menu());
        items.push(MenuItem::Separator);
        items.push(self.action("Quit", "application-exit", true, Command::Quit));

        items
    }
}
