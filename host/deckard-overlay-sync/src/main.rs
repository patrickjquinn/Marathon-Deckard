use deckard_overlay_sync::Paths;

fn main() {
    if let Err(error) = deckard_overlay_sync::sync(&Paths::from_env()) {
        eprintln!("deckard-overlay-sync: {error}");
        std::process::exit(1);
    }
}
