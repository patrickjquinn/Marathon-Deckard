use std::process::ExitCode;

use deckard_host_tools::migrate::{self, Direction, Scope};

const USAGE: &str = "usage: deckard-migrate forward|rollback [--user]";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let (direction, scope) = match arguments[..] {
        ["forward"] => (Direction::Forward, Scope::System),
        ["rollback"] => (Direction::Rollback, Scope::System),
        ["forward", "--user"] => (Direction::Forward, Scope::User),
        ["rollback", "--user"] => (Direction::Rollback, Scope::User),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if scope == Scope::System && !rustix::process::geteuid().is_root() {
        eprintln!("deckard-migrate: the system migration needs root; pass --user for yours");
        return ExitCode::from(2);
    }
    match migrate::run(direction, scope) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("deckard-migrate: {error}");
            ExitCode::FAILURE
        }
    }
}
