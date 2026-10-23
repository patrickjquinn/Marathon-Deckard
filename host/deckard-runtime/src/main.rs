const USAGE: &str = "usage: deckard-runtime session start|stop";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args[..] {
        ["session", "start"] => deckard_runtime::session_start(),
        ["session", "stop"] => deckard_runtime::session_stop(),
        _ => Err(USAGE.into()),
    };
    if let Err(error) = result {
        eprintln!("deckard-runtime: {error}");
        std::process::exit(1);
    }
}
