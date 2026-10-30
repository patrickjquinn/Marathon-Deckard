//! Keep the runtime's host shares aligned with the foreground app, and page
//! out the Android apps that are not in front once the foreground has been
//! still for a minute (again every ten while it stays). The root manager
//! accepts these requests only from this authenticated session process.

use std::time::Duration;

use async_io::Timer;
use deckard_launch::session::ContainerManagerProxy;
use futures_lite::StreamExt;
use zbus::runtime::traits::TaskHandle as _;
use zbus::{Connection, Proxy};

const IDLE: Duration = Duration::from_secs(60);
const AGAIN: Duration = Duration::from_secs(600);

/// The Android package Marathon shows, or "" when it shows none.
fn front_package(app: &str) -> &str {
    app.strip_prefix("deckard.").unwrap_or_default()
}

fn profile(app: &str) -> &'static str {
    if app.starts_with("deckard.") {
        "foreground"
    } else {
        "background"
    }
}

pub(crate) fn start(session: &Connection, system: &Connection) {
    let spawner = session.clone();
    let session = session.clone();
    let system = system.clone();
    let task = async move {
        if let Err(error) = follow(&session, &system).await {
            eprintln!("deckard-runtime: foreground resource policy: {error}");
        }
        // Unknown foreground must not retain a stale boost or background
        // penalty. The next session will read the Shell's current app again.
        if let Ok(root) = ContainerManagerProxy::new(&system).await {
            let _ = root.set_resource_profile("normal").await;
        }
    };
    spawner.spawn("foreground-resources", task).detach();
}

async fn follow(session: &Connection, system: &Connection) -> zbus::Result<()> {
    let root = ContainerManagerProxy::new(system).await?;
    root.set_resource_profile("normal").await?;
    let shell = Proxy::new(
        session,
        "org.marathonos.Shell",
        "/org/marathonos/Shell/Navigation",
        "org.marathonos.Shell.Navigation1",
    )
    .await?;
    // Subscribe first: a session can start with Android already in front.
    let mut changes = shell.receive_signal("ForegroundAppChanged").await?;
    // Unknown until the Shell answers: nothing is paged out on a guess.
    let mut front = match shell.call::<_, _, String>("GetForegroundApp", &()).await {
        Ok(app) => {
            root.set_resource_profile(profile(&app)).await?;
            Some(app)
        }
        Err(error) => {
            eprintln!(
                "deckard-runtime: initial foreground unavailable; following changes: {error}"
            );
            None
        }
    };
    let mut delay = IDLE;
    loop {
        let idle = async {
            match &front {
                Some(_) => Timer::after(delay).await,
                None => futures_lite::future::pending().await,
            };
            None
        };
        let Some(change) =
            futures_lite::future::or(async { Some(changes.next().await) }, idle).await
        else {
            let app = front.as_deref().unwrap_or_default();
            if let Err(error) = root.page_out_background(front_package(app)).await {
                eprintln!("deckard-runtime: cannot page out idle Android apps: {error}");
            }
            delay = AGAIN;
            continue;
        };
        let Some(change) = change else {
            return Ok(());
        };
        if let Ok(app) = change.body().deserialize::<String>() {
            root.set_resource_profile(profile(&app)).await?;
            front = Some(app);
            delay = IDLE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_foreground_identity_controls_runtime_shares() {
        assert_eq!(profile("deckard.com.zhiliaoapp.musically"), "foreground");
        assert_eq!(profile("shoal-pilot"), "background");
        assert_eq!(profile(""), "background");
        assert_eq!(profile("org.example.deckard.Editor"), "background");
    }
}

#[cfg(test)]
mod bus_tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    struct Bus(Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    struct Shell;
    #[zbus::interface(name = "org.marathonos.Shell.Navigation1")]
    impl Shell {
        fn get_foreground_app(&self) -> &str {
            "deckard.com.zhiliaoapp.musically"
        }
    }

    struct Root(async_channel::Sender<String>);
    #[zbus::interface(name = "org.marathonos.Deckard.ContainerManager")]
    impl Root {
        fn set_resource_profile(&self, profile: &str) {
            let _ = self.0.try_send(profile.to_owned());
        }
    }

    #[test]
    fn existing_foreground_and_later_changes_reach_the_root_manager() -> crate::Result<()> {
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()?;
        let output = daemon.stdout.take().ok_or("bus address pipe missing")?;
        let _daemon = Bus(daemon);
        let mut address = String::new();
        BufReader::new(output).read_line(&mut address)?;
        let address = address.trim();
        async_io::block_on(async {
            let (sent, received) = async_channel::bounded(8);
            let _root = zbus::connection::Builder::address(address)
                .name("org.marathonos.Deckard.Container")
                .serve_at("/ContainerManager", Root(sent))
                .build()
                .await?;
            let shell = zbus::connection::Builder::address(address)
                .name("org.marathonos.Shell")
                .serve_at("/org/marathonos/Shell/Navigation", Shell)
                .build()
                .await?;
            let client = zbus::connection::Builder::address(address).build().await?;
            let watch = async { follow(&client, &client).await.map_err(Into::into) };
            let check = async {
                assert_eq!(received.recv().await?, "normal");
                assert_eq!(received.recv().await?, "foreground");
                for (app, expected) in [
                    ("shoal-pilot", "background"),
                    ("deckard.com.example.app", "foreground"),
                ] {
                    shell
                        .emit_signal(
                            None::<&str>,
                            "/org/marathonos/Shell/Navigation",
                            "org.marathonos.Shell.Navigation1",
                            "ForegroundAppChanged",
                            &app,
                        )
                        .await?;
                    assert_eq!(received.recv().await?, expected);
                }
                Ok(())
            };
            let exercise = futures_lite::future::or(watch, check);
            futures_lite::future::or(exercise, async {
                async_io::Timer::after(Duration::from_secs(5)).await;
                Err("foreground policy did not reach the root manager".into())
            })
            .await
        })
    }
}
