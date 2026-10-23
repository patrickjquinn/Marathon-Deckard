# deckard-mpris

Inactive native Rust candidate for `host/deckardd/src/mpris.cpp` and `mpris.h`.
It consumes validated Android `media`/`media_gone` bridge messages and publishes
one MPRIS2 service **and one distinct session-bus connection** per active
MediaSession. Commands are returned as `media_cmd` JSON to the embedding bridge.
The legacy bus name, `waydroid.<package>` desktop entry, object path and
`media_cmd` keys intentionally remain unchanged until the coordinated desktop
identity migration. It accepts a private bus address for contract tests.

Run from the repository root:

```sh
cargo test -p deckard-mpris --locked
cargo clippy -p deckard-mpris --all-targets --locked -- -D warnings
```

The private-bus fixtures require `dbus-daemon`. They cover simultaneous
players, D-Bus properties, metadata, commands, seek guards, property changes,
seek notifications and owner release.

## Cutover gaps

- `deckardd` does not call this library yet; its C++ MPRIS adapter stays active.
  The future Rust bridge must pass incoming messages to `handle`, drain the
  command receiver to the Android socket, and call `clear` on disconnect.
- The future daemon must own the cache directory and coordinate its lifecycle
  with notification art and the eventual `waydroid.` to `deckard.` desktop
  identity migration. This candidate only deletes art files it created.
- Private-bus tests do not prove actual Android MediaSession commands or Shell
  media controls on the Hackberry Pi. Device round trips are required before
  switching the live service.
