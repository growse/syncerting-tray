//! Control of the `syncthing.service` user unit over the systemd D-Bus API.
//!
//! Talking to systemd directly rather than shelling out to `systemctl --user`
//! avoids process spawning per poll and gives typed errors back.

use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;
use zbus::zvariant::OwnedObjectPath;

pub const UNIT_NAME: &str = "syncthing.service";

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Manager {
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    fn restart_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    fn load_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath>;
    fn reload(&self) -> zbus::Result<()>;

    fn enable_unit_files(
        &self,
        files: &[&str],
        runtime: bool,
        force: bool,
    ) -> zbus::Result<(bool, Vec<(String, String, String)>)>;

    fn disable_unit_files(
        &self,
        files: &[&str],
        runtime: bool,
    ) -> zbus::Result<Vec<(String, String, String)>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Unit",
    default_service = "org.freedesktop.systemd1"
)]
trait Unit {
    #[zbus(property)]
    fn active_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn load_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn unit_file_state(&self) -> zbus::Result<String>;
}

pub struct Systemd {
    connection: zbus::Connection,
    manager: ManagerProxy<'static>,
}

impl Systemd {
    pub async fn connect() -> Result<Self> {
        let connection = zbus::Connection::session()
            .await
            .context("connecting to the session bus")?;
        let manager = ManagerProxy::new(&connection)
            .await
            .context("connecting to the systemd user manager")?;
        Ok(Self {
            connection,
            manager,
        })
    }

    /// The session bus connection, shared so the single-instance name is held
    /// for the lifetime of the process.
    pub fn connection(&self) -> &zbus::Connection {
        &self.connection
    }

    /// Current unit state. A missing unit is reported as `installed: false`
    /// rather than an error, since that is an expected first-run condition.
    pub async fn state(&self) -> Result<crate::model::UnitState> {
        let path = match self.manager.load_unit(UNIT_NAME).await {
            Ok(path) => path,
            // systemd refuses to load a unit with no fragment on disk.
            Err(_) => return Ok(crate::model::UnitState::default()),
        };

        let unit = UnitProxy::builder(&self.connection)
            .path(path)?
            .build()
            .await?;

        let load_state = unit.load_state().await.unwrap_or_default();
        if load_state == "not-found" {
            return Ok(crate::model::UnitState::default());
        }

        Ok(crate::model::UnitState {
            installed: true,
            active_state: unit.active_state().await.unwrap_or_default(),
            file_state: unit.unit_file_state().await.unwrap_or_default(),
        })
    }

    pub async fn start(&self) -> Result<()> {
        self.manager
            .start_unit(UNIT_NAME, "replace")
            .await
            .with_context(|| format!("starting {UNIT_NAME}"))?;
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        self.manager
            .stop_unit(UNIT_NAME, "replace")
            .await
            .with_context(|| format!("stopping {UNIT_NAME}"))?;
        Ok(())
    }

    pub async fn restart(&self) -> Result<()> {
        self.manager
            .restart_unit(UNIT_NAME, "replace")
            .await
            .with_context(|| format!("restarting {UNIT_NAME}"))?;
        Ok(())
    }

    pub async fn set_enabled(&self, enabled: bool) -> Result<()> {
        if enabled {
            self.manager
                .enable_unit_files(&[UNIT_NAME], false, true)
                .await
                .with_context(|| format!("enabling {UNIT_NAME}"))?;
        } else {
            self.manager
                .disable_unit_files(&[UNIT_NAME], false)
                .await
                .with_context(|| format!("disabling {UNIT_NAME}"))?;
        }
        self.reload().await
    }

    pub async fn reload(&self) -> Result<()> {
        self.manager
            .reload()
            .await
            .context("reloading the systemd user manager")?;
        Ok(())
    }
}

/// Where a user unit we write ourselves belongs.
pub fn user_unit_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .ok_or_else(|| anyhow!("cannot determine the user config directory"))?
        .join("systemd/user");
    Ok(dir.join(UNIT_NAME))
}

/// Locate the syncthing binary so the unit gets an absolute `ExecStart`.
fn find_syncthing() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("SYNCTHING_BINARY") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "SYNCTHING_BINARY points at {}, which is not a file",
            path.display()
        ));
    }

    let path_var = std::env::var("PATH").unwrap_or_default();
    std::env::split_paths(&path_var)
        .map(|dir| dir.join("syncthing"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            anyhow!(
                "no `syncthing` binary on PATH - install Syncthing first, or set SYNCTHING_BINARY"
            )
        })
}

/// Write `~/.config/systemd/user/syncthing.service` if it is not already there.
///
/// Mirrors the unit Syncthing ships upstream. Returns the path written.
pub fn write_unit_file() -> Result<PathBuf> {
    let binary = find_syncthing()?;
    let path = user_unit_path()?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let contents = format!(
        r#"[Unit]
Description=Syncthing - Open Source Continuous File Synchronization
Documentation=man:syncthing(1)
StartLimitIntervalSec=60
StartLimitBurst=4

[Service]
ExecStart={binary} serve --no-browser --no-restart --logflags=0
Restart=on-failure
RestartSec=1
SuccessExitStatus=3 4
RestartForceExitStatus=3 4

# Hardening
ProtectSystem=full
PrivateTmp=true
SystemCallArchitectures=native
MemoryDenyWriteExecute=true
NoNewPrivileges=true

[Install]
WantedBy=default.target
"#,
        binary = binary.display()
    );

    std::fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}
