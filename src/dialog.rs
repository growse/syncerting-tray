//! Error dialogs for failed user actions.
//!
//! A tray menu closes the moment it is clicked, so an error recorded only in the
//! menu is invisible until the user reopens it. Anything the user explicitly asked
//! for therefore reports failure in a dialog as well.
//!
//! Rather than pull in a toolkit, this shells out to whichever dialog helper the
//! desktop provides, preferring KDE's own.

use std::path::PathBuf;
use std::sync::OnceLock;

/// The dialog helpers we know how to drive, in order of preference.
const HELPERS: [&str; 3] = ["kdialog", "zenity", "notify-send"];

fn helper() -> Option<&'static (String, PathBuf)> {
    static HELPER: OnceLock<Option<(String, PathBuf)>> = OnceLock::new();
    HELPER
        .get_or_init(|| {
            let path_var = std::env::var("PATH").unwrap_or_default();
            let dirs: Vec<PathBuf> = std::env::split_paths(&path_var).collect();

            HELPERS.iter().find_map(|name| {
                dirs.iter()
                    .map(|dir| dir.join(name))
                    .find(|candidate| candidate.is_file())
                    .map(|path| ((*name).to_string(), path))
            })
        })
        .as_ref()
}

fn arguments(helper: &str, title: &str, message: &str) -> Vec<String> {
    match helper {
        "kdialog" => vec![
            "--title".into(),
            title.into(),
            "--error".into(),
            message.into(),
        ],
        "zenity" => vec![
            "--error".into(),
            format!("--title={title}"),
            format!("--text={message}"),
        ],
        // Not a dialog, but the last resort still needs to be visible.
        _ => vec![
            "--urgency=critical".into(),
            "--icon=state-error".into(),
            title.into(),
            message.into(),
        ],
    }
}

/// Show `message` in an error dialog, without blocking the caller.
///
/// Failure to show the dialog is itself only logged; there is nowhere left to
/// report it, and it must never take down the tray.
pub fn show_error(title: &str, message: &str) {
    let Some((name, path)) = helper() else {
        eprintln!(
            "syncerting-tray: cannot show a dialog, none of {} are installed",
            HELPERS.join(", ")
        );
        return;
    };

    let args = arguments(name, title, message);
    let mut command = tokio::process::Command::new(path);
    command.args(args).stdin(std::process::Stdio::null());

    match command.spawn() {
        // The child is reaped in the background so it does not become a zombie,
        // and so a modal dialog does not stall the worker while it is open.
        Ok(mut child) => {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
        Err(error) => eprintln!("syncerting-tray: could not run {}: {error}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kdialog_gets_a_title_and_an_error_body() {
        let args = arguments("kdialog", "Syncerting Tray", "boom");
        assert_eq!(args, vec!["--title", "Syncerting Tray", "--error", "boom"]);
    }

    #[test]
    fn zenity_takes_its_text_as_joined_flags() {
        let args = arguments("zenity", "Syncerting Tray", "boom");
        assert_eq!(
            args,
            vec!["--error", "--title=Syncerting Tray", "--text=boom"]
        );
    }

    #[test]
    fn notify_send_is_the_fallback() {
        let args = arguments("notify-send", "Syncerting Tray", "boom");
        assert_eq!(
            args,
            vec![
                "--urgency=critical",
                "--icon=state-error",
                "Syncerting Tray",
                "boom"
            ]
        );
    }

    #[test]
    fn a_message_that_looks_like_a_flag_is_not_treated_as_one() {
        // Arguments are passed as a vector, never through a shell, so a message
        // beginning with a dash stays a message.
        let args = arguments("kdialog", "T", "--yesno pwned");
        assert_eq!(args.last().unwrap(), "--yesno pwned");
        assert_eq!(args.len(), 4);
    }
}
