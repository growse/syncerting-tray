//! Syncthing REST API client.
//!
//! Only the handful of endpoints the tray needs are modelled; responses are
//! deserialised into just the fields used, so upstream additions are ignored.

use crate::config::SyncthingConfig;
use crate::model::FolderState;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::Duration;

/// How long the events endpoint is allowed to hold a request open.
const EVENT_TIMEOUT_SECS: u64 = 55;

pub struct SyncthingClient {
    http: reqwest::Client,
    /// Separate client for long-polling, which must outlive the normal timeout.
    events_http: reqwest::Client,
    config: SyncthingConfig,
}

#[derive(Debug, Deserialize)]
struct VersionResponse {
    version: String,
}

#[derive(Debug, Deserialize)]
struct FolderConfig {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    path: String,
}

#[derive(Debug, Deserialize)]
struct DeviceConfig {
    #[serde(rename = "deviceID")]
    device_id: String,
}

#[derive(Debug, Deserialize)]
struct ConnectionsResponse {
    #[serde(default)]
    connections: std::collections::HashMap<String, ConnectionEntry>,
}

#[derive(Debug, Deserialize)]
struct ConnectionEntry {
    #[serde(default)]
    connected: bool,
}

#[derive(Debug, Deserialize)]
struct DbStatus {
    #[serde(default)]
    state: String,
    #[serde(default)]
    #[serde(rename = "globalBytes")]
    global_bytes: u64,
    #[serde(default)]
    #[serde(rename = "needBytes")]
    need_bytes: u64,
    #[serde(default)]
    errors: u64,
}

#[derive(Debug, Deserialize)]
pub struct Event {
    pub id: u64,
    #[serde(rename = "type")]
    pub event_type: String,
}

/// A full snapshot of what the tray displays.
pub struct Snapshot {
    pub folders: Vec<FolderState>,
    pub devices_connected: usize,
    pub devices_total: usize,
    pub version: String,
}

impl SyncthingClient {
    pub fn new(config: SyncthingConfig) -> Result<Self> {
        let build = |timeout: Duration| {
            reqwest::Client::builder()
                .timeout(timeout)
                // Syncthing's GUI certificate is self-signed and, for a loopback
                // connection, there is nothing meaningful to verify it against.
                .danger_accept_invalid_certs(config.insecure_tls)
                .build()
        };

        Ok(Self {
            http: build(Duration::from_secs(10)).context("building the HTTP client")?,
            events_http: build(Duration::from_secs(EVENT_TIMEOUT_SECS + 10))
                .context("building the events HTTP client")?,
            config,
        })
    }

    pub fn web_ui_url(&self) -> &str {
        &self.config.base_url
    }

    fn request(
        &self,
        client: &reqwest::Client,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        client
            .request(method, format!("{}{}", self.config.base_url, path))
            .header("X-API-Key", &self.config.api_key)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .request(&self.http, reqwest::Method::GET, path)
            .send()
            .await
            .with_context(|| format!("GET {path}"))?
            .error_for_status()
            .with_context(|| format!("GET {path}"))?;
        response
            .json()
            .await
            .with_context(|| format!("decoding the response to GET {path}"))
    }

    async fn post(&self, path: &str) -> Result<()> {
        self.request(&self.http, reqwest::Method::POST, path)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?
            .error_for_status()
            .with_context(|| format!("POST {path}"))?;
        Ok(())
    }

    /// Cheap reachability probe used before a full refresh.
    pub async fn ping(&self) -> Result<String> {
        let version: VersionResponse = self.get_json("/rest/system/version").await?;
        Ok(version.version)
    }

    /// Fetch folders, per-folder sync status and device connectivity.
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let version = self.ping().await?;

        let folder_configs: Vec<FolderConfig> = self.get_json("/rest/config/folders").await?;
        let device_configs: Vec<DeviceConfig> = self.get_json("/rest/config/devices").await?;
        let connections: ConnectionsResponse = self.get_json("/rest/system/connections").await?;

        // The local device appears in the device list but never in connections,
        // so exclude it from the "connected" ratio.
        let local_id: String = {
            #[derive(Deserialize)]
            struct Status {
                #[serde(rename = "myID")]
                my_id: String,
            }
            let status: Status = self.get_json("/rest/system/status").await?;
            status.my_id
        };

        let remote_devices: Vec<&DeviceConfig> = device_configs
            .iter()
            .filter(|d| d.device_id != local_id)
            .collect();

        let devices_connected = remote_devices
            .iter()
            .filter(|d| {
                connections
                    .connections
                    .get(&d.device_id)
                    .is_some_and(|c| c.connected)
            })
            .count();

        let mut folders = Vec::with_capacity(folder_configs.len());
        for folder in folder_configs {
            let status: DbStatus = self
                .get_json(&format!("/rest/db/status?folder={}", urlencode(&folder.id)))
                .await
                .unwrap_or(DbStatus {
                    state: "unknown".into(),
                    global_bytes: 0,
                    need_bytes: 0,
                    errors: 0,
                });

            folders.push(FolderState {
                id: folder.id,
                label: folder.label,
                paused: folder.paused,
                path: folder.path,
                state: if folder.paused {
                    "paused".into()
                } else {
                    status.state.clone()
                },
                completion: completion_percent(status.global_bytes, status.need_bytes),
                errors: status.errors,
            });
        }

        folders.sort_by_key(|f| f.display_name().to_lowercase());

        Ok(Snapshot {
            folders,
            devices_connected,
            devices_total: remote_devices.len(),
            version,
        })
    }

    /// Long-poll the event stream. Returns the events seen and the new cursor.
    ///
    /// `since` of 0 asks Syncthing for only the latest event, which avoids
    /// replaying the entire backlog on startup.
    pub async fn poll_events(&self, since: u64) -> Result<(Vec<Event>, u64)> {
        let path = format!("/rest/events?since={since}&timeout={EVENT_TIMEOUT_SECS}");
        let response = self
            .request(&self.events_http, reqwest::Method::GET, &path)
            .send()
            .await
            .context("polling the event stream")?
            .error_for_status()
            .context("polling the event stream")?;

        let events: Vec<Event> = response
            .json()
            .await
            .context("decoding the event stream response")?;

        let cursor = events.iter().map(|e| e.id).max().unwrap_or(since);
        Ok((events, cursor))
    }

    pub async fn rescan_all(&self) -> Result<()> {
        self.post("/rest/db/scan").await
    }

    pub async fn rescan_folder(&self, folder: &str) -> Result<()> {
        self.post(&format!("/rest/db/scan?folder={}", urlencode(folder)))
            .await
    }

    /// Pause or resume every remote device at once.
    pub async fn set_all_paused(&self, paused: bool) -> Result<()> {
        let endpoint = if paused {
            "/rest/system/pause"
        } else {
            "/rest/system/resume"
        };
        self.post(endpoint).await
    }

    /// Pause or resume a single folder by patching its config entry.
    pub async fn set_folder_paused(&self, folder: &str, paused: bool) -> Result<()> {
        let path = format!("/rest/config/folders/{}", urlencode(folder));
        self.request(&self.http, reqwest::Method::PATCH, &path)
            .json(&serde_json::json!({ "paused": paused }))
            .send()
            .await
            .with_context(|| format!("PATCH {path}"))?
            .error_for_status()
            .with_context(|| format!("PATCH {path}"))?;
        Ok(())
    }
}

/// Percentage of a folder that is in sync.
fn completion_percent(global_bytes: u64, need_bytes: u64) -> f64 {
    if global_bytes == 0 {
        return 100.0;
    }
    let done = global_bytes.saturating_sub(need_bytes) as f64;
    (done / global_bytes as f64 * 100.0).clamp(0.0, 100.0)
}

/// Percent-encode a query parameter value.
///
/// Folder IDs are user-chosen and may contain spaces or punctuation, so they
/// cannot be interpolated raw.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Event types that mean the displayed state may have changed.
pub fn is_interesting(event_type: &str) -> bool {
    matches!(
        event_type,
        "StateChanged"
            | "FolderSummary"
            | "FolderCompletion"
            | "FolderErrors"
            | "FolderPaused"
            | "FolderResumed"
            | "FolderScanProgress"
            | "DeviceConnected"
            | "DeviceDisconnected"
            | "DevicePaused"
            | "DeviceResumed"
            | "ConfigSaved"
            | "DownloadProgress"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_handles_empty_folders() {
        assert_eq!(completion_percent(0, 0), 100.0);
    }

    #[test]
    fn completion_is_proportional() {
        assert_eq!(completion_percent(100, 25), 75.0);
        assert_eq!(completion_percent(100, 0), 100.0);
    }

    #[test]
    fn completion_never_goes_negative() {
        assert_eq!(completion_percent(100, 500), 0.0);
    }

    #[test]
    fn urlencode_escapes_reserved_characters() {
        assert_eq!(urlencode("my folder"), "my%20folder");
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencode("plain-id_1.2~3"), "plain-id_1.2~3");
    }

    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Minimal HTTP server that replies to exact request targets with canned JSON.
    ///
    /// Returns the base URL and a shared record of the API keys it was sent, so
    /// tests can assert the auth header is applied.
    async fn stub_server(
        routes: Vec<(&'static str, &'static str)>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen_keys = Arc::new(Mutex::new(Vec::new()));
        let keys = seen_keys.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let routes = routes.clone();
                let keys = keys.clone();

                tokio::spawn(async move {
                    // Requests are read one byte at a time; slow, but it keeps
                    // the parser trivial and these payloads are tiny.
                    loop {
                        let mut head = Vec::new();
                        let mut byte = [0u8; 1];
                        loop {
                            match socket.read(&mut byte).await {
                                Ok(0) | Err(_) => return,
                                Ok(_) => {
                                    head.push(byte[0]);
                                    if head.ends_with(b"\r\n\r\n") {
                                        break;
                                    }
                                }
                            }
                        }

                        let head = String::from_utf8_lossy(&head).to_string();
                        let target = head
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("")
                            .to_string();

                        if let Some(key) = head
                            .lines()
                            .find(|l| l.to_ascii_lowercase().starts_with("x-api-key:"))
                            .and_then(|l| l.split_once(':'))
                            .map(|(_, v)| v.trim().to_string())
                        {
                            keys.lock().unwrap().push(key);
                        }

                        let response = match routes.iter().find(|(path, _)| *path == target) {
                            Some((_, body)) => format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                body.len(),
                                body
                            ),
                            None => {
                                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string()
                            }
                        };

                        if socket.write_all(response.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });

        (format!("http://{addr}"), seen_keys)
    }

    fn client_for(base_url: String) -> SyncthingClient {
        SyncthingClient::new(SyncthingConfig {
            base_url,
            api_key: "test-key".into(),
            insecure_tls: false,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn snapshot_builds_state_from_the_rest_api() {
        let (url, seen_keys) = stub_server(vec![
            ("/rest/system/version", r#"{"version":"v1.29.2"}"#),
            (
                "/rest/config/folders",
                r#"[{"id":"docs","label":"Documents","paused":false,"path":"/home/x/Documents"},
                    {"id":"code","label":"","paused":true,"path":"/home/x/code"}]"#,
            ),
            (
                "/rest/config/devices",
                r#"[{"deviceID":"LOCAL"},{"deviceID":"REMOTE1"},{"deviceID":"REMOTE2"}]"#,
            ),
            (
                "/rest/system/connections",
                r#"{"connections":{"REMOTE1":{"connected":true},"REMOTE2":{"connected":false}}}"#,
            ),
            ("/rest/system/status", r#"{"myID":"LOCAL"}"#),
            (
                "/rest/db/status?folder=docs",
                r#"{"state":"syncing","globalBytes":1000,"needBytes":250,"errors":0}"#,
            ),
            (
                "/rest/db/status?folder=code",
                r#"{"state":"idle","globalBytes":500,"needBytes":0,"errors":0}"#,
            ),
        ])
        .await;

        let snapshot = client_for(url).snapshot().await.unwrap();

        assert_eq!(snapshot.version, "v1.29.2");
        // The local device is excluded from the connected ratio.
        assert_eq!(snapshot.devices_total, 2);
        assert_eq!(snapshot.devices_connected, 1);

        // Sorted case-insensitively by display name, so "code" precedes "Documents".
        assert_eq!(snapshot.folders.len(), 2);
        let code = &snapshot.folders[0];
        let docs = &snapshot.folders[1];

        // An empty label falls back to the folder id.
        assert_eq!(code.display_name(), "code");
        assert!(code.paused);
        // A paused folder reports "paused" regardless of its database state.
        assert_eq!(code.state, "paused");
        assert_eq!(code.summary(), "paused");

        assert_eq!(docs.display_name(), "Documents");
        assert_eq!(docs.state, "syncing");
        assert_eq!(docs.completion, 75.0);
        assert_eq!(docs.summary(), "syncing 75%");

        let keys = seen_keys.lock().unwrap();
        assert!(!keys.is_empty());
        assert!(keys.iter().all(|k| k == "test-key"));
    }

    #[tokio::test]
    async fn a_failing_folder_status_does_not_sink_the_whole_snapshot() {
        // /rest/db/status is deliberately absent, so it 404s.
        let (url, _) = stub_server(vec![
            ("/rest/system/version", r#"{"version":"v1.29.2"}"#),
            (
                "/rest/config/folders",
                r#"[{"id":"docs","label":"Documents","paused":false,"path":"/tmp"}]"#,
            ),
            ("/rest/config/devices", r#"[{"deviceID":"LOCAL"}]"#),
            ("/rest/system/connections", r#"{"connections":{}}"#),
            ("/rest/system/status", r#"{"myID":"LOCAL"}"#),
        ])
        .await;

        let snapshot = client_for(url).snapshot().await.unwrap();

        assert_eq!(snapshot.folders.len(), 1);
        assert_eq!(snapshot.folders[0].state, "unknown");
    }

    #[tokio::test]
    async fn an_unreachable_api_is_an_error_not_a_panic() {
        // Port 1 on loopback has nothing listening.
        let result = client_for("http://127.0.0.1:1".into()).snapshot().await;
        assert!(result.is_err());
    }
}
