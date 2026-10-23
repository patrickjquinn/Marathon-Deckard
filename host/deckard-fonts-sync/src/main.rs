use std::path::PathBuf;

use deckard_fonts_sync::{LinuxWhiteouts, Paths, sync};

fn main() {
    let share = std::env::var_os("DECKARD_FONTS")
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from("/usr/share/deckard/fonts"), PathBuf::from);
    let paths = Paths {
        share,
        host_fonts: PathBuf::from("/usr/share/fonts"),
        overlay_system: PathBuf::from("/var/lib/waydroid/overlay/system"),
        state: PathBuf::from("/var/lib/waydroid/deckard-fonts.list"),
    };
    if let Err(error) = sync(&paths, &mut LinuxWhiteouts) {
        eprintln!("deckard-fonts-sync: {error}");
        std::process::exit(1);
    }
}
