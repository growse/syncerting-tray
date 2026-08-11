//! The state the tray renders, and the commands the menu can issue.

/// Whether the systemd unit exists, and what it is doing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnitState {
    /// False when no `syncthing.service` user unit is installed at all.
    pub installed: bool,
    /// `active`, `inactive`, `failed`, `activating`, ...
    pub active_state: String,
    /// `enabled`, `disabled`, `static`, ...
    pub file_state: String,
}

impl UnitState {
    pub fn is_active(&self) -> bool {
        self.active_state == "active"
    }

    pub fn is_enabled(&self) -> bool {
        self.file_state.starts_with("enabled")
    }

    pub fn has_failed(&self) -> bool {
        self.active_state == "failed"
    }
}

/// Aggregate sync state of a single folder.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderState {
    pub id: String,
    pub label: String,
    pub paused: bool,
    pub path: String,
    /// `idle`, `syncing`, `scanning`, `error`, ...
    pub state: String,
    /// 0.0 - 100.0
    pub completion: f64,
    pub errors: u64,
}

impl FolderState {
    pub fn display_name(&self) -> &str {
        if self.label.is_empty() {
            &self.id
        } else {
            &self.label
        }
    }

    /// Short right-hand status used in the menu label.
    pub fn summary(&self) -> String {
        if self.paused {
            "paused".into()
        } else if self.errors > 0 {
            format!(
                "{} error{}",
                self.errors,
                if self.errors == 1 { "" } else { "s" }
            )
        } else if self.state == "syncing" || self.state == "scanning" {
            format!("{} {:.0}%", self.state, self.completion)
        } else if self.completion >= 99.995 {
            "up to date".into()
        } else {
            format!("{:.0}%", self.completion)
        }
    }
}

/// Reachability of the REST API, kept separate from the unit state because a
/// running unit can still be unreachable while it starts up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiState {
    /// No config.xml, or no API key in it.
    NotConfigured(String),
    Unreachable(String),
    Connected,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub unit: UnitState,
    pub api: ApiState,
    pub folders: Vec<FolderState>,
    pub devices_connected: usize,
    pub devices_total: usize,
    pub version: Option<String>,
    /// Set when an action fails, shown in the menu until the next success.
    pub last_error: Option<String>,
    /// True while a start/stop/restart is in flight.
    pub busy: bool,
    /// Whether the tray itself starts with the desktop session.
    pub autostart: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            unit: UnitState::default(),
            api: ApiState::Unreachable("starting up".into()),
            folders: Vec::new(),
            devices_connected: 0,
            devices_total: 0,
            version: None,
            last_error: None,
            busy: false,
            autostart: false,
        }
    }
}

/// Overall health, which drives the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Stopped,
    Starting,
    Error,
    Syncing,
    Paused,
    Ok,
}

impl Health {
    /// Every state, so the icons can be pre-rendered.
    pub const ALL: [Health; 6] = [
        Health::Stopped,
        Health::Starting,
        Health::Error,
        Health::Syncing,
        Health::Paused,
        Health::Ok,
    ];

    /// Breeze fallback, used for menu entries and if pixmap rendering fails.
    pub fn icon_name(self) -> &'static str {
        match self {
            Health::Stopped => "state-offline",
            Health::Starting => "state-sync",
            Health::Error => "state-error",
            Health::Syncing => "state-sync",
            Health::Paused => "state-pause",
            Health::Ok => "state-ok",
        }
    }
}

impl AppState {
    pub fn health(&self) -> Health {
        if self.unit.has_failed() {
            return Health::Error;
        }
        if !self.unit.installed || !self.unit.is_active() {
            return Health::Stopped;
        }
        match &self.api {
            // The unit is up but the API is not answering yet, which is normal
            // for the first second or two after a start.
            ApiState::Unreachable(_) => return Health::Starting,
            ApiState::NotConfigured(_) => return Health::Error,
            ApiState::Connected => {}
        }
        if self
            .folders
            .iter()
            .any(|f| f.errors > 0 || f.state == "error")
        {
            return Health::Error;
        }
        if self
            .folders
            .iter()
            .any(|f| !f.paused && (f.state == "syncing" || f.state == "scanning"))
        {
            return Health::Syncing;
        }
        if !self.folders.is_empty() && self.folders.iter().all(|f| f.paused) {
            return Health::Paused;
        }
        Health::Ok
    }

    /// One-line status shown at the top of the menu and in the tooltip.
    pub fn headline(&self) -> String {
        if !self.unit.installed {
            return "Syncthing: not installed as a user service".into();
        }
        if self.unit.has_failed() {
            return "Syncthing: service failed".into();
        }
        if !self.unit.is_active() {
            return "Syncthing: stopped".into();
        }
        match &self.api {
            ApiState::NotConfigured(why) => format!("Syncthing: {why}"),
            ApiState::Unreachable(_) => "Syncthing: starting…".into(),
            ApiState::Connected => format!(
                "Syncthing: running ({}/{} device{} connected)",
                self.devices_connected,
                self.devices_total,
                if self.devices_total == 1 { "" } else { "s" }
            ),
        }
    }

    /// True when every device is paused, so the menu can offer "Resume All".
    pub fn all_paused(&self) -> bool {
        !self.folders.is_empty() && self.folders.iter().all(|f| f.paused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(state: &str, paused: bool, errors: u64) -> FolderState {
        FolderState {
            id: "f".into(),
            label: "Folder".into(),
            paused,
            path: "/tmp".into(),
            state: state.into(),
            completion: 100.0,
            errors,
        }
    }

    fn running(folders: Vec<FolderState>) -> AppState {
        AppState {
            unit: UnitState {
                installed: true,
                active_state: "active".into(),
                file_state: "enabled".into(),
            },
            api: ApiState::Connected,
            folders,
            ..Default::default()
        }
    }

    #[test]
    fn a_missing_unit_reads_as_stopped() {
        let state = AppState::default();
        assert_eq!(state.health(), Health::Stopped);
        assert_eq!(state.health().icon_name(), "state-offline");
    }

    #[test]
    fn a_failed_unit_outranks_everything_else() {
        let mut state = running(vec![]);
        state.unit.active_state = "failed".into();
        assert_eq!(state.health(), Health::Error);
    }

    #[test]
    fn a_live_unit_with_no_api_yet_is_starting() {
        let mut state = running(vec![]);
        state.api = ApiState::Unreachable("connection refused".into());
        assert_eq!(state.health(), Health::Starting);
    }

    #[test]
    fn folder_errors_outrank_syncing() {
        let state = running(vec![folder("syncing", false, 0), folder("idle", false, 3)]);
        assert_eq!(state.health(), Health::Error);
    }

    #[test]
    fn syncing_outranks_idle() {
        let state = running(vec![folder("idle", false, 0), folder("syncing", false, 0)]);
        assert_eq!(state.health(), Health::Syncing);
        assert_eq!(state.health().icon_name(), "state-sync");
    }

    #[test]
    fn paused_needs_every_folder_paused() {
        let all = running(vec![folder("paused", true, 0), folder("paused", true, 0)]);
        assert_eq!(all.health(), Health::Paused);
        assert!(all.all_paused());

        let some = running(vec![folder("paused", true, 0), folder("idle", false, 0)]);
        assert_eq!(some.health(), Health::Ok);
        assert!(!some.all_paused());
    }

    #[test]
    fn a_paused_folder_is_not_counted_as_syncing() {
        // A folder can still report a stale "syncing" state while paused.
        let state = running(vec![folder("syncing", true, 0)]);
        assert_eq!(state.health(), Health::Paused);
    }

    #[test]
    fn no_folders_at_all_is_still_ok() {
        let state = running(vec![]);
        assert_eq!(state.health(), Health::Ok);
        assert!(!state.all_paused());
    }

    #[test]
    fn headline_reports_the_connected_device_ratio() {
        let mut state = running(vec![]);
        state.devices_connected = 1;
        state.devices_total = 3;
        assert_eq!(
            state.headline(),
            "Syncthing: running (1/3 devices connected)"
        );

        state.devices_total = 1;
        assert_eq!(
            state.headline(),
            "Syncthing: running (1/1 device connected)"
        );
    }

    #[test]
    fn enabled_detection_tolerates_the_runtime_suffix() {
        let mut unit = UnitState {
            installed: true,
            active_state: "active".into(),
            file_state: "enabled".into(),
        };
        assert!(unit.is_enabled());
        // systemd reports "enabled-runtime" for transiently enabled units.
        unit.file_state = "enabled-runtime".into();
        assert!(unit.is_enabled());
        unit.file_state = "disabled".into();
        assert!(!unit.is_enabled());
    }
}

/// Actions requested from the menu. Menu callbacks are synchronous, so they
/// post one of these to the worker task rather than doing the work inline.
#[derive(Debug, Clone)]
pub enum Command {
    Start,
    Stop,
    Restart,
    SetEnabled(bool),
    /// Autostart for the tray itself, distinct from [`Command::SetEnabled`],
    /// which governs the Syncthing service.
    SetAutostart(bool),
    InstallUnit,
    RescanAll,
    SetAllPaused(bool),
    OpenWebUi,
    OpenFolder(String),
    Refresh,
    Quit,
}

impl Command {
    /// Human-readable name for the action, used to title an error dialog.
    pub fn label(&self) -> String {
        match self {
            Command::Start => "Start Syncthing".into(),
            Command::Stop => "Stop Syncthing".into(),
            Command::Restart => "Restart Syncthing".into(),
            Command::SetEnabled(true) => "Enable Start Syncthing at Login".into(),
            Command::SetEnabled(false) => "Disable Start Syncthing at Login".into(),
            Command::SetAutostart(true) => "Enable Start Tray at Login".into(),
            Command::SetAutostart(false) => "Disable Start Tray at Login".into(),
            Command::InstallUnit => "Install User Service".into(),
            Command::RescanAll => "Rescan All".into(),
            Command::SetAllPaused(true) => "Pause All".into(),
            Command::SetAllPaused(false) => "Resume All".into(),
            Command::OpenWebUi => "Open Web UI".into(),
            Command::OpenFolder(path) => format!("Open {path}"),
            Command::Refresh => "Refresh".into(),
            Command::Quit => "Quit".into(),
        }
    }
}
