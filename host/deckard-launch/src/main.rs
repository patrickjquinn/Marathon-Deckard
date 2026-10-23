use deckard_launch::session::NativeBackend;
use deckard_launch::{get_property, launch_app, launch_intent, set_property, Result};
use std::env;
use std::path::Path;

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let config =
        env::var("DECKARD_CONFIG").unwrap_or_else(|_| "/var/lib/waydroid/waydroid.cfg".into());
    let session_unit =
        env::var("DECKARD_SESSION_UNIT").unwrap_or_else(|_| "deckard-session.service".into());
    let mut backend = NativeBackend::new(Path::new(&config), &session_unit)?;
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["app", "launch", package] => launch_app(&mut backend, package),
        ["app", "intent", action, uri] => launch_intent(&mut backend, action, uri),
        ["prop", "get", key] => {
            let value = get_property(&mut backend, key)?;
            if !value.is_empty() {
                println!("{value}");
            }
            Ok(())
        }
        ["prop", "set", key, value] => set_property(&mut backend, key, value),
        _ => Err("usage: deckard-launch app launch PACKAGE | app intent ACTION URI | prop get KEY | prop set KEY VALUE".into()),
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("deckard-launch: {err}");
        std::process::exit(1);
    }
}
