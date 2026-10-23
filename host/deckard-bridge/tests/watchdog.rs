use std::env;
use std::fs::{self, File};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use deckard_bridge::watchdog::{self, BootWatchdog};
use deckard_core::runtime_state::{BOOT_TIMEOUT_MS, SHELL_BUS_NAME};
use zbus::blocking::Connection;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const BOOT_MS: i64 = BOOT_TIMEOUT_MS as i64;
const QUIET: Duration = Duration::from_millis(300);
const SOON: Duration = Duration::from_secs(3);

fn now_ms() -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

fn booted(marker: &Path, ago: Duration) -> TestResult {
    fs::write(marker, "1")?;
    File::options()
        .write(true)
        .open(marker)?
        .set_modified(SystemTime::now() - ago)?;
    Ok(())
}

#[test]
fn restarts_a_stalled_boot_once_until_a_boot_completes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let marker = dir.path().join("booted");
    booted(&marker, Duration::from_secs(600))?;
    let (restarted, restarts) = mpsc::channel();
    let evidence = ["sh", "-c", "echo stuck"].map(Into::into).to_vec();
    let watchdog = BootWatchdog::spawn(marker.clone(), Some(evidence), move || {
        let _ = restarted.send(());
    });

    watchdog.set_active_since(now_ms()? - BOOT_MS);
    restarts.recv_timeout(SOON)?;

    // The restarted session stalls as well: reported, not restarted.
    watchdog.set_active_since(0);
    watchdog.set_active_since(now_ms()? - BOOT_MS + 1);
    assert!(restarts.recv_timeout(QUIET * 2).is_err());

    // A completed boot re-arms the restart.
    watchdog.set_active_since(0);
    let start = now_ms()? - 1_000;
    booted(&marker, Duration::ZERO)?;
    watchdog.set_active_since(start - BOOT_MS);
    assert!(restarts.recv_timeout(QUIET).is_err());
    watchdog.set_active_since(0);
    booted(&marker, Duration::from_secs(600))?;
    watchdog.set_active_since(now_ms()? - BOOT_MS);
    restarts.recv_timeout(SOON)?;
    Ok(())
}

#[test]
fn restarts_android_once_the_shell_comes_back() -> TestResult {
    if env::var_os("DECKARD_BRIDGE_PRIVATE_BUS").is_none() {
        return Ok(());
    }
    let (restarted, restarts) = mpsc::channel();
    let bus = Connection::session()?;
    thread::spawn(move || {
        watchdog::follow_shell(&bus, move || {
            let _ = restarted.send(());
        })
    });
    let shell = Connection::session()?;
    shell.request_name(SHELL_BUS_NAME)?;
    assert!(restarts.recv_timeout(QUIET).is_err());
    for _ in 0..3 {
        shell.release_name(SHELL_BUS_NAME)?;
        shell.request_name(SHELL_BUS_NAME)?;
        if restarts.recv_timeout(SOON).is_ok() {
            // A crash loop restarts Android once, not on every Shell start.
            shell.release_name(SHELL_BUS_NAME)?;
            shell.request_name(SHELL_BUS_NAME)?;
            assert!(restarts.recv_timeout(SOON).is_err());
            return Ok(());
        }
    }
    Err("the Shell's return never restarted Android".into())
}
