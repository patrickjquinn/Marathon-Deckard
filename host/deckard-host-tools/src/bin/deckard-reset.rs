use std::process::ExitCode;

use deckard_host_tools::reset;

fn main() -> ExitCode {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    match reset::run(&arguments, &reset::Environment::from_process()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("deckard-reset: {error}");
            ExitCode::from(error.status())
        }
    }
}
