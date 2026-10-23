use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let path = arguments
        .next()
        .ok_or("usage: deckard-open-apk <file.apk>")?;
    if arguments.next().is_some() {
        return Err("expected exactly one APK path".into());
    }
    let arguments = deckard_host_tools::apk::arguments(Path::new(&path))?;
    Err(Command::new("busctl").args(arguments).exec().into())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("deckard-open-apk: {error}");
            ExitCode::from(2)
        }
    }
}
