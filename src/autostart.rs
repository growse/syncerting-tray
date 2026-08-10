//! Autostart for the tray itself, via an XDG autostart desktop entry.
//!
//! This is deliberately not the mechanism used for Syncthing: the daemon is a
//! background service and belongs to systemd, whereas the tray is a desktop
//! application that should start with the desktop session and stop with it.
//! `~/.config/autostart` is what the desktop reads for that.
//!
//! The entry is generated rather than copied from `resources/`, because
//! `cargo install` puts only the binary on the system and the source tree will
//! not be there at runtime.

use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "syncerting-tray.desktop";

/// Location of the autostart entry.
pub fn desktop_file_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .ok_or_else(|| anyhow!("cannot determine the user config directory"))?
        .join("autostart");
    Ok(dir.join(FILE_NAME))
}

/// Whether the tray is set to start with the desktop session.
///
/// A `Hidden=true` entry counts as disabled: that is how the spec, and several
/// desktop settings panels, switch an entry off without deleting it.
pub fn is_enabled() -> bool {
    let Ok(path) = desktop_file_path() else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    !text
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case("hidden=true"))
}

/// Quote a path for a desktop entry `Exec` key if it needs it.
///
/// Reserved characters would otherwise be parsed as argument separators, so a
/// binary living under a path with a space in it would silently fail to start.
fn quote_exec(path: &Path) -> String {
    let text = path.display().to_string();
    if text
        .chars()
        .any(|c| c.is_whitespace() || r#""'\`$"#.contains(c))
    {
        format!("\"{}\"", text.replace('\\', r"\\").replace('"', "\\\""))
    } else {
        text
    }
}

/// Build the desktop entry, pointing at whichever binary is running.
///
/// Using the running binary's own path means this works the same whether the
/// tray was installed to `~/.local/bin`, installed by `cargo install`, or is
/// being run straight out of `target/`.
fn desktop_entry() -> Result<String> {
    let exe = std::env::current_exe().context("locating the running executable")?;
    // Resolve symlinks so the entry keeps working if the link is later moved.
    let exe = exe.canonicalize().unwrap_or(exe);
    let mut exec = quote_exec(&exe);

    // A non-default icon style is part of how the user configured this tray, so
    // carry it across; otherwise the autostarted tray would look different from
    // the one they set up.
    if let Ok(style) = std::env::var("SYNCERTING_ICON_STYLE")
        && !style.trim().is_empty()
    {
        exec = format!("env SYNCERTING_ICON_STYLE={} {exec}", style.trim());
    }

    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Syncerting Tray\n\
         GenericName=Syncthing Tray\n\
         Comment=Monitor and control Syncthing from the system tray\n\
         Exec={exec}\n\
         Icon=syncerting-tray-ok\n\
         Terminal=false\n\
         Categories=Network;FileTransfer;\n\
         Keywords=syncthing;sync;\n\
         X-GNOME-Autostart-enabled=true\n"
    ))
}

/// Turn autostart on or off.
///
/// Disabling removes the entry rather than marking it hidden, so nothing is left
/// behind pointing at a binary that may later be uninstalled.
pub fn set_enabled(enabled: bool) -> Result<()> {
    let path = desktop_file_path()?;

    if !enabled {
        match std::fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            // Already absent is the desired state, not a failure.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("removing {}", path.display()));
            }
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, desktop_entry()?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_points_at_the_running_binary() {
        let entry = desktop_entry().expect("builds an entry");
        let exe = std::env::current_exe().unwrap();
        let exe = exe.canonicalize().unwrap_or(exe);

        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("Type=Application"));
        assert!(
            entry.contains(&exe.display().to_string()),
            "entry does not reference {}",
            exe.display()
        );
    }

    #[test]
    fn paths_needing_quotes_get_them() {
        assert_eq!(
            quote_exec(Path::new("/usr/bin/syncerting-tray")),
            "/usr/bin/syncerting-tray"
        );
        assert_eq!(
            quote_exec(Path::new("/home/a b/syncerting-tray")),
            "\"/home/a b/syncerting-tray\""
        );
    }

    #[test]
    fn a_hidden_entry_reads_as_disabled() {
        // Mirrors is_enabled's parsing without touching the real config file.
        let hidden = "[Desktop Entry]\nType=Application\nHidden=true\n";
        let live = "[Desktop Entry]\nType=Application\n";
        let disabled = |text: &str| {
            text.lines()
                .any(|line| line.trim().eq_ignore_ascii_case("hidden=true"))
        };
        assert!(disabled(hidden));
        assert!(!disabled(live));
    }
}
