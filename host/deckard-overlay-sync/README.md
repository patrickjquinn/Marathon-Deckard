# deckard-overlay-sync

Standalone Rust replacement for `host/packaging/deckard-overlay-sync`. This crate
is intentionally outside the repository's root Cargo workspace and does not
change packaging until the binary is explicitly adopted.

Build and verify with `cargo test --manifest-path host/deckard-overlay-sync/Cargo.toml`
and `cargo clippy --manifest-path host/deckard-overlay-sync/Cargo.toml --all-targets -- -D warnings`.

The binary defaults to `/usr/share/deckard` and `/var/lib/waydroid`.
`DECKARD_SHARE_DIR` and `DECKARD_WAYDROID_DIR` override these paths for staging
or tests. Real whiteout creation requires permission to create `c 0:0` device
nodes in the Waydroid overlay. The tests inject a fake whiteout backend and do
not require root.
