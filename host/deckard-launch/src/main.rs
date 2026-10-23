use std::env;
use std::path::Path;

use deckard_launch::Result;
use deckard_launch::session::NativeBackend;

const USAGE: &str = "usage: deckard-launch app launch PACKAGE | app intent ACTION URI | prop get KEY | prop set KEY VALUE";

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let config =
        env::var("DECKARD_CONFIG").unwrap_or_else(|_| "/var/lib/waydroid/waydroid.cfg".into());
    let session_unit =
        env::var("DECKARD_SESSION_UNIT").unwrap_or_else(|_| "deckard-session.service".into());
    let mut backend = NativeBackend::new(Path::new(&config), &session_unit)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["app", "launch", package] => deckard_launch::launch_app(&mut backend, package),
        ["app", "intent", action, uri] => deckard_launch::launch_intent(&mut backend, action, uri),
        ["prop", "get", key] => {
            let value = deckard_launch::get_property(&mut backend, key)?;
            if !value.is_empty() {
                println!("{value}");
            }
            Ok(())
        }
        ["prop", "set", key, value] => deckard_launch::set_property(&mut backend, key, value),
        _ => Err(USAGE.into()),
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("deckard-launch: {err}");
        std::process::exit(1);
    }
}
