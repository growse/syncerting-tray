//! A Syncthing tray application for KDE Plasma.
//!
//! Presents a StatusNotifierItem that reflects Syncthing's sync state, controls
//! the `syncthing.service` systemd user unit, and hands off anything deeper to
//! Syncthing's own web UI.

mod autostart;
mod client;
mod config;
mod dialog;
mod icons;
mod instance;
mod model;
mod tray;
mod unit;

use anyhow::Result;
use client::SyncthingClient;
use config::SyncthingConfig;
use ksni::TrayMethods;
use model::{ApiState, AppState, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tray::SyncthingTray;
use unit::Systemd;

/// How often the worker wakes to consider refreshing.
const TICK: Duration = Duration::from_secs(1);
/// Upper bound between full refreshes, even with no events at all.
const MAX_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
/// Fallback when Syncthing has never been configured.
const DEFAULT_WEB_UI: &str = "http://127.0.0.1:8384";
/// Prefix for dialog titles.
const APP_TITLE: &str = "Syncthing Tray";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let systemd = Systemd::connect().await?;

    // Claimed before the tray is registered, so a second instance never puts a
    // duplicate icon in the tray even briefly. The name is held by the systemd
    // connection, which lives until the process exits.
    if instance::acquire(systemd.connection()).await? == instance::Acquired::AlreadyRunning {
        eprintln!("syncerting-tray: another instance is already running");
        return Ok(());
    }

    // The monochrome icons have to exist on disk before the tray advertises a
    // path to them; a failure here only costs the monochrome style, so it is
    // logged rather than fatal.
    let style = icons::Style::from_env();
    let theme_path = match style {
        icons::Style::Monochrome => match icons::install_monochrome_theme() {
            Ok(path) => path.display().to_string(),
            Err(error) => {
                eprintln!("syncerting-tray: {error:#}");
                String::new()
            }
        },
        icons::Style::Colour => String::new(),
    };

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = SyncthingTray::new(tx.clone(), style, theme_path)
        .spawn()
        .await
        .map_err(|e| anyhow::anyhow!("registering the tray item: {e}"))?;

    let worker = Worker {
        systemd,
        client: None,
        events_task: None,
        state: AppState::default(),
        tx,
    };

    worker.run(rx, handle).await;
    Ok(())
}

struct Worker {
    systemd: Systemd,
    client: Option<Arc<SyncthingClient>>,
    events_task: Option<tokio::task::JoinHandle<()>>,
    state: AppState,
    tx: UnboundedSender<Command>,
}

impl Worker {
    async fn run(
        mut self,
        mut rx: UnboundedReceiver<Command>,
        handle: ksni::Handle<SyncthingTray>,
    ) {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut dirty = true;
        let mut last_refresh = Instant::now() - MAX_REFRESH_INTERVAL;

        loop {
            tokio::select! {
                command = rx.recv() => {
                    let Some(command) = command else { break };
                    if matches!(command, Command::Quit) {
                        handle.shutdown().await;
                        break;
                    }
                    // Commands that mutate state get an immediate refresh; a bare
                    // Refresh only marks the state stale so bursts of events coalesce.
                    if !matches!(command, Command::Refresh) {
                        self.handle_command(command, &handle).await;
                        last_refresh = Instant::now() - MAX_REFRESH_INTERVAL;
                    }
                    dirty = true;
                }
                _ = ticker.tick() => {
                    if dirty && last_refresh.elapsed() >= TICK
                        || last_refresh.elapsed() >= MAX_REFRESH_INTERVAL
                    {
                        self.refresh().await;
                        dirty = false;
                        last_refresh = Instant::now();
                        if !self.publish(&handle).await {
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Push the current state into the tray. Returns false once the tray is gone.
    async fn publish(&self, handle: &ksni::Handle<SyncthingTray>) -> bool {
        let state = self.state.clone();
        handle
            .update(move |tray: &mut SyncthingTray| tray.state = state)
            .await
            .is_some()
    }

    async fn handle_command(&mut self, command: Command, handle: &ksni::Handle<SyncthingTray>) {
        // Service operations block for as long as systemd takes, so show a busy
        // state while they are in flight.
        let is_service_op = matches!(
            command,
            Command::Start
                | Command::Stop
                | Command::Restart
                | Command::SetEnabled(_)
                | Command::SetAutostart(_)
                | Command::InstallUnit
        );
        if is_service_op {
            self.state.busy = true;
            self.publish(handle).await;
        }

        // Captured before dispatch consumes the command, so a failure can say
        // which action it was.
        let label = command.label();
        let outcome = self.dispatch(command).await;

        self.state.busy = false;
        match outcome {
            Ok(()) => self.state.last_error = None,
            Err(error) => {
                // The root cause is more useful in a one-line menu entry than the
                // full anyhow chain.
                let root = error.root_cause().to_string();
                eprintln!("syncerting-tray: {error:#}");
                self.state.last_error = Some(root);
                // Only user-initiated actions land here; background refresh
                // failures are recorded in the menu instead, so a Syncthing that
                // is merely down cannot produce a stream of dialogs.
                dialog::show_error(&format!("{APP_TITLE}: {label}"), &format!("{error:#}"));
            }
        }
    }

    async fn dispatch(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Start => self.systemd.start().await,
            Command::Stop => self.systemd.stop().await,
            Command::Restart => self.systemd.restart().await,
            Command::SetEnabled(enabled) => self.systemd.set_enabled(enabled).await,
            Command::SetAutostart(enabled) => autostart::set_enabled(enabled),

            Command::InstallUnit => {
                let path = unit::write_unit_file()?;
                eprintln!("syncerting-tray: wrote {}", path.display());
                self.systemd.reload().await?;
                self.systemd.set_enabled(true).await?;
                self.systemd.start().await
            }

            Command::OpenWebUi => {
                let url = self
                    .client
                    .as_ref()
                    .map(|c| c.web_ui_url().to_string())
                    .unwrap_or_else(|| DEFAULT_WEB_UI.to_string());
                open::that_detached(&url).map_err(|e| anyhow::anyhow!("opening {url}: {e}"))
            }

            Command::OpenFolder(path) => {
                open::that_detached(&path).map_err(|e| anyhow::anyhow!("opening {path}: {e}"))
            }

            Command::RescanAll => {
                self.with_client(|c| async move { c.rescan_all().await })
                    .await
            }

            Command::RescanFolder(id) => {
                self.with_client(|c| async move { c.rescan_folder(&id).await })
                    .await
            }

            Command::SetAllPaused(paused) => {
                self.with_client(|c| async move { c.set_all_paused(paused).await })
                    .await
            }

            Command::SetFolderPaused(id, paused) => {
                self.with_client(|c| async move { c.set_folder_paused(&id, paused).await })
                    .await
            }

            Command::Refresh | Command::Quit => Ok(()),
        }
    }

    /// Run an API call, surfacing a clear error when there is no usable client.
    async fn with_client<F, Fut>(&mut self, f: F) -> Result<()>
    where
        F: FnOnce(Arc<SyncthingClient>) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        self.ensure_client();
        let client = self
            .client
            .clone()
            .ok_or_else(|| anyhow::anyhow!("not connected to Syncthing"))?;
        f(client).await
    }

    /// Build the REST client once Syncthing's config exists, and start the
    /// event stream that keeps the tray responsive between ticks.
    fn ensure_client(&mut self) {
        if self.client.is_some() {
            return;
        }

        match SyncthingConfig::discover().and_then(SyncthingClient::new) {
            Ok(client) => {
                let client = Arc::new(client);
                self.client = Some(client.clone());
                self.state.api = ApiState::Unreachable("connecting".into());

                let tx = self.tx.clone();
                if let Some(old) = self.events_task.take() {
                    old.abort();
                }
                self.events_task = Some(tokio::spawn(event_loop(client, tx)));
            }
            Err(error) => {
                self.state.api = ApiState::NotConfigured(error.root_cause().to_string());
            }
        }
    }

    async fn refresh(&mut self) {
        self.state.unit = self.systemd.state().await.unwrap_or_default();
        self.state.autostart = autostart::is_enabled();

        self.ensure_client();
        let Some(client) = self.client.clone() else {
            // ensure_client already recorded why.
            self.state.folders.clear();
            self.state.devices_connected = 0;
            self.state.devices_total = 0;
            return;
        };

        match client.snapshot().await {
            Ok(snapshot) => {
                self.state.api = ApiState::Connected;
                self.state.folders = snapshot.folders;
                self.state.devices_connected = snapshot.devices_connected;
                self.state.devices_total = snapshot.devices_total;
                self.state.version = Some(snapshot.version);
            }
            Err(error) => {
                self.state.api = ApiState::Unreachable(error.root_cause().to_string());
                self.state.folders.clear();
                self.state.devices_connected = 0;
                self.state.devices_total = 0;

                // A stale API key survives a Syncthing reinstall, so drop the
                // client and let the next tick re-read config.xml.
                if !self.state.unit.is_active() {
                    self.client = None;
                    if let Some(task) = self.events_task.take() {
                        task.abort();
                    }
                }
            }
        }
    }
}

/// Long-poll Syncthing's event stream, nudging the worker when something changes.
async fn event_loop(client: Arc<SyncthingClient>, tx: UnboundedSender<Command>) {
    // 0 asks for only the most recent event rather than the whole backlog.
    let mut since = 0u64;

    loop {
        match client.poll_events(since).await {
            Ok((events, cursor)) => {
                since = cursor;
                if events.iter().any(|e| client::is_interesting(&e.event_type))
                    && tx.send(Command::Refresh).is_err()
                {
                    return;
                }
            }
            Err(_) => {
                // Syncthing is probably down; back off rather than spin.
                tokio::time::sleep(Duration::from_secs(5)).await;
                if tx.send(Command::Refresh).is_err() {
                    return;
                }
            }
        }
    }
}
