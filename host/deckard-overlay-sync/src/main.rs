use std::path::PathBuf;

use deckard_overlay_sync::Paths;

fn main() {
    let paths = Paths {
        share: PathBuf::from("/usr/share/deckard"),
        waydroid: PathBuf::from("/var/lib/waydroid"),
    };
    if let Err(error) = deckard_overlay_sync::sync(&paths) {
        eprintln!("deckard-overlay-sync: {error}");
        std::process::exit(1);
    }
}
