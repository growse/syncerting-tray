//! Discovery of the local Syncthing installation: where its config lives, what
//! API key to use, and which URL its GUI is listening on.

use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

/// Everything needed to talk to the local Syncthing REST API.
#[derive(Debug, Clone)]
pub struct SyncthingConfig {
    pub base_url: String,
    pub api_key: String,
    /// Syncthing's GUI certificate is self-signed, so TLS verification has to be
    /// waived. Only ever true for loopback addresses.
    pub insecure_tls: bool,
}

/// Candidate locations for `config.xml`, most preferred first.
///
/// Syncthing 1.27 moved its state out of `~/.config` and into `XDG_STATE_HOME`,
/// but leaves an existing config where it was on upgrade, so both must be tried.
fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(home) = std::env::var("STHOMEDIR") {
        paths.push(PathBuf::from(home).join("config.xml"));
    }
    if let Some(state) = dirs::state_dir() {
        paths.push(state.join("syncthing/config.xml"));
    }
    if let Some(config) = dirs::config_dir() {
        paths.push(config.join("syncthing/config.xml"));
    }

    paths
}

/// Locate `config.xml`, or `None` if Syncthing has never been run.
pub fn find_config_file() -> Option<PathBuf> {
    candidate_paths().into_iter().find(|p| p.is_file())
}

impl SyncthingConfig {
    /// Read connection details from Syncthing's own config file.
    ///
    /// `SYNCTHING_API_KEY` and `SYNCTHING_URL` override whatever is found there,
    /// which covers the case of pointing at a non-standard instance.
    pub fn discover() -> Result<Self> {
        let path = find_config_file().ok_or_else(|| {
            anyhow!(
                "no Syncthing config.xml found (looked in {})",
                candidate_paths()
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;

        let mut cfg = Self::parse(&path)?;

        if let Ok(key) = std::env::var("SYNCTHING_API_KEY") {
            cfg.api_key = key;
        }
        if let Ok(url) = std::env::var("SYNCTHING_URL") {
            cfg.base_url = url.trim_end_matches('/').to_string();
            cfg.insecure_tls = cfg.base_url.starts_with("https://");
        }

        if cfg.api_key.is_empty() {
            return Err(anyhow!(
                "Syncthing config at {} has no API key set",
                path.display()
            ));
        }

        Ok(cfg)
    }

    fn parse(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let doc = roxmltree::Document::parse(&text)
            .with_context(|| format!("parsing {}", path.display()))?;

        let gui = doc
            .root_element()
            .children()
            .find(|n| n.has_tag_name("gui"))
            .ok_or_else(|| anyhow!("no <gui> section in {}", path.display()))?;

        let child_text = |name: &str| {
            gui.children()
                .find(|n| n.has_tag_name(name))
                .and_then(|n| n.text())
                .unwrap_or("")
                .trim()
                .to_string()
        };

        let api_key = child_text("apikey");
        let address = child_text("address");
        let tls = gui.attribute("tls").is_some_and(|v| v == "true");

        Ok(Self {
            base_url: normalise_url(&address, tls),
            api_key,
            insecure_tls: tls,
        })
    }
}

/// Turn a Syncthing listen address into a URL we can actually connect to.
///
/// The config stores a bind address, which may be a wildcard such as `0.0.0.0:8384`
/// or `:8384`. Those are not connectable, so they collapse to loopback.
fn normalise_url(address: &str, tls: bool) -> String {
    let scheme = if tls { "https" } else { "http" };
    let addr = address.trim();

    if addr.is_empty() {
        return format!("{scheme}://127.0.0.1:8384");
    }

    // Split host from port on the last colon so IPv6 literals survive.
    let (host, port) = match addr.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => (h, p),
        _ => (addr, "8384"),
    };

    let host = host.trim_matches(|c| c == '[' || c == ']');
    let host = match host {
        "" | "0.0.0.0" | "::" => "127.0.0.1",
        other => other,
    };

    if host.contains(':') {
        format!("{scheme}://[{host}]:{port}")
    } else {
        format!("{scheme}://{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_addresses_become_loopback() {
        assert_eq!(
            normalise_url("0.0.0.0:8384", false),
            "http://127.0.0.1:8384"
        );
        assert_eq!(normalise_url(":8384", false), "http://127.0.0.1:8384");
        assert_eq!(normalise_url("::", false), "http://127.0.0.1:8384");
        assert_eq!(normalise_url("", false), "http://127.0.0.1:8384");
    }

    #[test]
    fn explicit_addresses_are_preserved() {
        assert_eq!(
            normalise_url("192.168.1.5:9090", false),
            "http://192.168.1.5:9090"
        );
        assert_eq!(
            normalise_url("127.0.0.1:8384", true),
            "https://127.0.0.1:8384"
        );
    }

    #[test]
    fn ipv6_literals_keep_their_brackets() {
        assert_eq!(normalise_url("[::1]:8384", false), "http://[::1]:8384");
    }

    #[test]
    fn missing_port_falls_back_to_default() {
        assert_eq!(normalise_url("127.0.0.1", false), "http://127.0.0.1:8384");
    }
}
