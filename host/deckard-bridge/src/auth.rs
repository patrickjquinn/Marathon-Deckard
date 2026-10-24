//! Who a D-Bus caller is: its process, its Shell app, its executable.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Duration;

use zbus::fdo::DBusProxy;
use zbus::message::Header;

use crate::intents;

const MAX_HOPS: usize = 8;
const TIMEOUT: Duration = Duration::from_secs(5);

fn parent_of(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = stat.rsplit_once(')')?.1.trim_start();
    after_name.split_ascii_whitespace().nth(1)?.parse().ok()
}

/// The process id of the bus peer `sender`.
///
/// # Errors
/// The bus does not know `sender`.
pub async fn pid(bus: &zbus::Connection, sender: &str) -> zbus::Result<u32> {
    let dbus = DBusProxy::new(bus).await?;
    Ok(dbus
        .get_connection_unix_process_id(sender.try_into()?)
        .await?)
}

/// The Shell app id of `pid` or of its nearest ancestor that has one; empty
/// when none within eight hops does.
///
/// # Errors
/// The Shell did not answer.
pub async fn app(bus: &zbus::Connection, mut pid: u32) -> zbus::Result<String> {
    let (service, path, interface) = intents::NAVIGATION;
    let shell = zbus::Proxy::new(bus, service, path, interface).await?;
    for _ in 0..MAX_HOPS {
        if pid <= 1 {
            break;
        }
        let app: String = shell.call("AppIdForPid", &(pid,)).await?;
        if !app.is_empty() {
            return Ok(app);
        }
        pid = parent_of(pid).unwrap_or(0);
    }
    Ok(String::new())
}

/// Whether `pid` runs the executable at one of `paths`, by inode.
#[must_use]
pub fn runs_one_of(pid: u32, paths: &[PathBuf]) -> bool {
    let Ok(exe) = fs::metadata(format!("/proc/{pid}/exe")) else {
        return false;
    };
    let same = |path: &PathBuf| {
        fs::metadata(path).is_ok_and(|file| file.dev() == exe.dev() && file.ino() == exe.ino())
    };
    pid > 0 && paths.iter().any(same)
}

/// Whether the caller in `header` is the Settings app or one of its
/// children; false when that takes over 5 s.
pub async fn settings(bus: &zbus::Connection, header: Header<'_>) -> bool {
    let Some(sender) = header.sender() else {
        return false;
    };
    let app = async { app(bus, pid(bus, sender.as_str()).await?).await };
    let app = intents::within(TIMEOUT, app).await;
    matches!(app, Some(Ok(app)) if app == "settings")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_parent_from_proc_stat() {
        assert!(parent_of(std::process::id()).is_some());
        assert_eq!(parent_of(u32::MAX), None);
    }
}
