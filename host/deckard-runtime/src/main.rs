use std::env;
use std::io::{self, Write};

fn run() -> deckard_runtime::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [a, b] if a == "session" && b == "start" => deckard_runtime::session_start(),
        [a, b] if a == "session" && b == "stop" => deckard_runtime::session_stop(),
        [a, b] if a == "session" && b == "export" => deckard_runtime::export_current_user_apps(),
        [a, b, key] if a == "prop" && b == "get" => {
            println!("{}", deckard_runtime::property_get(key)?);
            Ok(())
        }
        [a, b, key, value] if a == "prop" && b == "set" => {
            deckard_runtime::property_set(key, value)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: deckard-runtime session start|stop|export | prop get KEY | prop set KEY VALUE",
        )
        .into()),
    }
}

fn main() {
    if let Err(error) = run() {
        let _ = writeln!(io::stderr().lock(), "deckard-runtime: {error}");
        std::process::exit(1);
    }
}
