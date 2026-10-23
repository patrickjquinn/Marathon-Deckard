use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use deckard_core::ids::PackageName;
use deckard_core::transport::{BridgeListener, BridgeSession};
use deckard_ime::{
    ImeState, SHELL_NAME, ShellBus, ShellCall, TEXT_INPUT_INTERFACE, TEXT_INPUT_PATH,
};
use zbus::blocking::{Connection, MessageIterator};
use zbus::{MatchRule, message};

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, PartialEq, Eq)]
enum ShellSignal {
    Commit(String, String, i32, i32),
    Key(String, String),
    Panel(String, i32, i32),
}

enum Event {
    Accepted(BridgeSession),
    Shell(ShellSignal),
    ShellAppeared,
    ShellListenerReady,
}

fn signal(message: &message::Message) -> Option<ShellSignal> {
    match message.header().member()?.as_str() {
        "CommitString" => {
            let (app, value, before, after) = message.body().deserialize().ok()?;
            Some(ShellSignal::Commit(app, value, before, after))
        }
        "Key" => {
            let (app, key) = message.body().deserialize().ok()?;
            Some(ShellSignal::Key(app, key))
        }
        "PanelChanged" => {
            let (app, covered, height) = message.body().deserialize().ok()?;
            Some(ShellSignal::Panel(app, covered, height))
        }
        _ => None,
    }
}

fn listen_shell(connection: Connection, sender: Sender<Event>) -> zbus::Result<()> {
    let rule = MatchRule::builder()
        .msg_type(message::Type::Signal)
        .sender(SHELL_NAME)?
        .path(TEXT_INPUT_PATH)?
        .interface(TEXT_INPUT_INTERFACE)?
        .build();
    let events = MessageIterator::for_match_rule(rule, &connection, Some(64))?;
    if sender.send(Event::ShellListenerReady).is_err() {
        return Ok(());
    }
    for event in events {
        match event {
            Ok(message) => {
                if let Some(event) = signal(&message)
                    && sender.send(Event::Shell(event)).is_err()
                {
                    break;
                }
            }
            Err(error) => eprintln!("deckard-ime: Shell signal error: {error}"),
        }
    }
    Ok(())
}

fn watch_shell(connection: Connection, sender: Sender<Event>) -> zbus::Result<()> {
    let rule = MatchRule::builder()
        .msg_type(message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .add_arg(SHELL_NAME)?
        .build();
    let changes = MessageIterator::for_match_rule(rule, &connection, Some(8))?;
    for change in changes {
        let Ok(message) = change else {
            continue;
        };
        let Ok((_, _, new_owner)) = message.body().deserialize::<(String, String, String)>() else {
            continue;
        };
        if !new_owner.is_empty() && sender.send(Event::ShellAppeared).is_err() {
            break;
        }
    }
    Ok(())
}

fn send_calls(bus: &ShellBus, calls: &[ShellCall]) {
    for call in calls {
        if let Err(error) = bus.call(call) {
            let method = match call {
                ShellCall::SetEditor(..) => "SetEditor",
                ShellCall::SetSurroundingText(..) => "SetSurroundingText",
                ShellCall::ShowPanel(..) => "ShowPanel",
                ShellCall::HidePanel(..) => "HidePanel",
                ShellCall::ClearEditor(..) => "ClearEditor",
            };
            eprintln!("deckard-ime: Shell {method} failed: {error}");
        }
    }
}

fn handle_event(
    event: Event,
    bus: &ShellBus,
    state: &mut ImeState,
    session: &mut Option<BridgeSession>,
) {
    match event {
        Event::Accepted(accepted) => {
            send_calls(bus, &state.disconnect());
            if let Err(error) = accepted.set_read_timeout(Some(Duration::from_millis(20))) {
                eprintln!("deckard-ime: socket timeout setup failed: {error}");
                return;
            }
            *session = Some(accepted);
        }
        Event::Shell(signal) => {
            let message = match signal {
                ShellSignal::Commit(app, value, before, after) => {
                    state.commit_string(&app, &value, before, after)
                }
                ShellSignal::Key(app, key) => state.key(&app, &key),
                ShellSignal::Panel(app, covered, height) => {
                    state.panel_changed(&app, covered, height)
                }
            };
            if let (Some(active), Some(message)) = (session.as_mut(), message)
                && let Err(error) = active.send_message(&message)
            {
                eprintln!("deckard-ime: Android send failed: {error}");
                *session = None;
                send_calls(bus, &state.disconnect());
            }
        }
        Event::ShellAppeared => {
            if let Err(error) = bus.register() {
                eprintln!("deckard-ime: Shell registration failed: {error}");
            }
        }
        Event::ShellListenerReady => {}
    }
}

fn run() -> Result<(), Error> {
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });
    let data = data_home.join("waydroid/data");
    let package = PackageName::try_from("org.marathonos.deckard.ime")?;
    let mut listener = BridgeListener::bind(
        data.join("deckard/ime.sock"),
        data.join("system/packages.list"),
        package,
    )?;
    let connection = Connection::session()?;
    let bus = ShellBus::new(connection.clone());
    let (sender, receiver): (Sender<Event>, Receiver<Event>) = mpsc::channel();
    let accept_sender = sender.clone();
    thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok(session) => {
                    if accept_sender.send(Event::Accepted(session)).is_err() {
                        break;
                    }
                }
                Err(error) => eprintln!("deckard-ime: peer rejected: {error}"),
            }
        }
    });
    let signal_sender = sender.clone();
    let signal_connection = connection.clone();
    thread::spawn(move || {
        if let Err(error) = listen_shell(signal_connection, signal_sender) {
            eprintln!("deckard-ime: Shell signal listener failed: {error}");
        }
    });
    thread::spawn(move || {
        if let Err(error) = watch_shell(connection, sender) {
            eprintln!("deckard-ime: Shell owner watcher failed: {error}");
        }
    });
    let mut state = ImeState::default();
    let mut session: Option<BridgeSession> = None;
    loop {
        match receiver.recv_timeout(Duration::from_secs(2))? {
            Event::ShellListenerReady => break,
            event => handle_event(event, &bus, &mut state, &mut session),
        }
    }
    if let Err(error) = bus.register() {
        eprintln!("deckard-ime: Shell is not available yet: {error}");
    }
    loop {
        while let Ok(event) = receiver.try_recv() {
            handle_event(event, &bus, &mut state, &mut session);
        }
        let Some(active) = session.as_mut() else {
            handle_event(receiver.recv()?, &bus, &mut state, &mut session);
            continue;
        };
        match active.read_message() {
            Ok(Some(message)) => {
                let calls = state.handle_android(&message);
                let started = calls
                    .iter()
                    .any(|call| matches!(call, ShellCall::SetEditor(..)));
                send_calls(&bus, &calls);
                if started
                    && let Some(panel) = state.current_panel()
                    && let Err(error) = active.send_message(&panel)
                {
                    eprintln!("deckard-ime: panel replay failed: {error}");
                    session = None;
                    send_calls(&bus, &state.disconnect());
                }
            }
            Ok(None) => {
                session = None;
                send_calls(&bus, &state.disconnect());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => {
                eprintln!("deckard-ime: Android read failed: {error}");
                session = None;
                send_calls(&bus, &state.disconnect());
            }
        }
    }
}

fn main() {
    if let Err(error) = run() {
        let _ = writeln!(io::stderr().lock(), "deckard-ime: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_bus_delivers_targeted_shell_signal() -> Result<(), Error> {
        if env::var_os("DECKARD_IME_PRIVATE_BUS").is_none() {
            return Ok(());
        }
        let shell = Connection::session()?;
        shell.request_name(SHELL_NAME)?;
        let adapter = Connection::session()?;
        let destination = adapter
            .unique_name()
            .ok_or_else(|| io::Error::other("adapter has no unique bus name"))?
            .to_string();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || listen_shell(adapter, sender));
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2))?,
            Event::ShellListenerReady
        ));
        shell.emit_signal(
            Some(destination.as_str()),
            TEXT_INPUT_PATH,
            TEXT_INPUT_INTERFACE,
            "CommitString",
            &("waydroid.org.example", "😀", 2_i32, 0_i32),
        )?;
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2))?,
            Event::Shell(ShellSignal::Commit(app, value, 2, 0))
                if app == "waydroid.org.example" && value == "😀"
        ));
        Ok(())
    }
}
