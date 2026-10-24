use std::path::PathBuf;
use std::process::ExitCode;

use deckard_host_tools::reset;

fn main() -> ExitCode {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let passwd = std::env::var_os("DECKARD_RESET_PASSWD").map(PathBuf::from);
    match reset::run(&arguments, passwd.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("deckard-reset: {error}");
            ExitCode::from(error.status())
        }
    }
}
