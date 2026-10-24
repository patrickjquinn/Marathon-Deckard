use std::path::PathBuf;

use deckard_host_tools::names;
use deckard_overlay_sync::Paths;

fn main() {
    let paths = Paths {
        share: PathBuf::from("/usr/share/deckard"),
        work: PathBuf::from(names::WORK),
    };
    if let Err(error) = deckard_overlay_sync::sync(&paths) {
        eprintln!("deckard-overlay-sync: {error}");
        std::process::exit(1);
    }
}
