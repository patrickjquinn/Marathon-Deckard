use std::path::{Path, PathBuf};

use deckard_fonts_sync::Paths;
use deckard_host_tools::names;

fn main() {
    let paths = Paths {
        share: PathBuf::from("/usr/share/deckard/fonts"),
        host_fonts: PathBuf::from("/usr/share/fonts"),
        overlay_system: Path::new(names::WORK).join("overlay/system"),
        state: Path::new(names::WORK).join("deckard-fonts.list"),
        config: PathBuf::from(names::CONFIG),
    };
    if let Err(error) = deckard_fonts_sync::sync(&paths) {
        eprintln!("deckard-fonts-sync: {error}");
        std::process::exit(1);
    }
}
