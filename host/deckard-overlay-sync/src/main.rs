use deckard_overlay_sync::{Paths, sync};

fn main() {
    if let Err(error) = sync(&Paths::from_env()) {
        eprintln!("deckard-overlay-sync: {error}");
        std::process::exit(1);
    }
}
