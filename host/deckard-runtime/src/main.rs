use deckard_runtime::Ended;

const USAGE: &str = "usage: deckard-runtime session start|stop";
/// deckard-session.service restarts on it without counting a failure.
const COMPOSITOR_RESTARTED: i32 = 75;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args[..] {
        ["session", "start"] => deckard_runtime::session_start(),
        ["session", "stop"] => deckard_runtime::session_stop().map(|()| Ended::Stopped),
        _ => Err(USAGE.into()),
    };
    match result {
        Ok(Ended::Stopped) => {}
        Ok(Ended::CompositorRestarted) => {
            eprintln!(
                "deckard-runtime: the Wayland compositor restarted; restarting the Android session"
            );
            std::process::exit(COMPOSITOR_RESTARTED);
        }
        Err(error) => {
            eprintln!("deckard-runtime: {error}");
            std::process::exit(1);
        }
    }
}
