use std::path::PathBuf;

use deckard_fonts_sync::Paths;

fn main() {
    let paths = Paths {
        share: PathBuf::from("/usr/share/deckard/fonts"),
        host_fonts: PathBuf::from("/usr/share/fonts"),
        overlay_system: PathBuf::from("/var/lib/waydroid/overlay/system"),
        state: PathBuf::from("/var/lib/waydroid/deckard-fonts.list"),
    };
    if let Err(error) = deckard_fonts_sync::sync(&paths) {
        eprintln!("deckard-fonts-sync: {error}");
        std::process::exit(1);
    }
}
