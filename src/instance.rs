//! Single-instance enforcement.
//!
//! Two trays would mean two icons, two event streams and two sets of dialogs for
//! the same failure. Ownership of a well-known bus name is used as the lock: the
//! bus grants it to exactly one connection, and releases it automatically when
//! that connection drops. There is no lock file to go stale if we are killed.

use anyhow::{Context, Result};
use zbus::fdo::{RequestNameFlags, RequestNameReply};

/// The well-known name whose ownership marks the running instance.
pub const BUS_NAME: &str = "dev.growse.SyncertingTray";

/// Outcome of trying to become the one running tray.
#[derive(Debug, PartialEq, Eq)]
pub enum Acquired {
    /// We hold the name; this is the only instance.
    Yes,
    /// Another instance already holds it.
    AlreadyRunning,
}

/// Try to claim the single-instance name on `connection`.
///
/// The name is held for as long as `connection` lives, so the caller must keep
/// it alive for the lifetime of the process.
pub async fn acquire(connection: &zbus::Connection) -> Result<Acquired> {
    // DoNotQueue makes this fail fast rather than silently waiting to inherit
    // the name when the other instance exits. ReplaceExisting is deliberately
    // not set: the instance already running should win, not the newcomer.
    let reply = connection
        .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
        .await;

    Ok(match reply {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => Acquired::Yes,
        Ok(RequestNameReply::Exists | RequestNameReply::InQueue) => Acquired::AlreadyRunning,
        // zbus turns a DoNotQueue refusal into an error rather than returning
        // `Exists`, so losing the race arrives here, not in the match above.
        Err(zbus::Error::NameTaken) => Acquired::AlreadyRunning,
        Err(error) => {
            return Err(error).with_context(|| format!("requesting the bus name {BUS_NAME}"));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_second_instance_loses_and_the_name_is_freed_on_disconnect() {
        let Ok(first) = zbus::Connection::session().await else {
            eprintln!("skipping: no session bus available");
            return;
        };

        assert_eq!(acquire(&first).await.unwrap(), Acquired::Yes);

        // Asking again on the same connection is still success, so a retry is
        // never mistaken for a second instance.
        assert_eq!(acquire(&first).await.unwrap(), Acquired::Yes);

        // A separate connection stands in for a second process.
        let second = zbus::Connection::session().await.unwrap();
        assert_eq!(acquire(&second).await.unwrap(), Acquired::AlreadyRunning);

        // Dropping the owner releases the name without any cleanup step, which
        // is what makes this robust against being killed.
        drop(first);
        let mut regained = Acquired::AlreadyRunning;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if acquire(&second).await.unwrap() == Acquired::Yes {
                regained = Acquired::Yes;
                break;
            }
        }
        assert_eq!(regained, Acquired::Yes);
    }
}
