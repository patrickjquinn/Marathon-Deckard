use std::fs;
use std::time::Duration;

use async_io::Timer;
use futures_lite::future;
use zbus::message::Header;

const MAX_HOPS: usize = 8;
const TIMEOUT: Duration = Duration::from_secs(5);

fn parent_of(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = stat.rsplit_once(')')?.1.trim_start();
    after_name.split_ascii_whitespace().nth(1)?.parse().ok()
}

async fn resolve(bus: &zbus::Connection, sender: &str) -> zbus::Result<bool> {
    let dbus = zbus::Proxy::new(
        bus,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await?;
    let mut pid: u32 = dbus.call("GetConnectionUnixProcessID", &(sender,)).await?;
    let shell = zbus::Proxy::new(
        bus,
        "org.marathonos.Shell",
        "/org/marathonos/Shell/Navigation",
        "org.marathonos.Shell.Navigation1",
    )
    .await?;
    for _ in 0..MAX_HOPS {
        if pid <= 1 {
            break;
        }
        let app: String = shell.call("AppIdForPid", &(pid,)).await?;
        if !app.is_empty() {
            return Ok(app == "settings");
        }
        pid = parent_of(pid).unwrap_or(0);
    }
    Ok(false)
}

/// Whether the caller in `header` is the Settings app or one of its
/// children; false when that takes over 5 s.
pub async fn settings(bus: &zbus::Connection, header: Header<'_>) -> bool {
    let Some(sender) = header.sender() else {
        return false;
    };
    future::or(
        async { resolve(bus, sender.as_str()).await.unwrap_or(false) },
        async {
            Timer::after(TIMEOUT).await;
            false
        },
    )
    .await
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
