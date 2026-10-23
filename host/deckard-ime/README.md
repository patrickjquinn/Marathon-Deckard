# deckard-ime

Native Rust IME adapter for the current DeckardIme Android app and Marathon
Shell `org.marathonos.Shell.TextInput1` contract. `deckard-bridge` embeds the
library for its IME channel (`host/deckard-bridge/src/ime.rs`); the Shell
admits the bridge for `text-input` through `deckard-bridge.service.json`.

The binary listens on `~/.local/share/waydroid/data/deckard/ime.sock` (or the
equivalent `XDG_DATA_HOME` path). It uses `deckard-core::transport` for framed
JSON, rate limits and `SO_PEERCRED` verification against
`org.marathonos.deckard.ime` in `packages.list`. It relays editor state to the
Shell with `waydroid.<package>` app ids and relays only validated, active-app
Shell signals back to Android. String lengths and offsets are UTF-16 code
units, as in Java and Qt. The Shell is re-registered when its bus name appears.

Run the fixtures with `cargo test -p deckard-ime --locked`.
Run the D-Bus signature test with:

```sh
dbus-run-session -- env DECKARD_IME_PRIVATE_BUS=1 cargo test -p deckard-ime --all-targets --locked
```

## Notes

- The standalone `deckard-ime` binary is not packaged. Do not run it alongside
  `deckard-bridge`: both would contend for `ime.sock` and the Shell
  registration.
- Android windows and desktop entries still use `waydroid.*` app ids and a
  Waydroid data path. Rename those atomically with the broader runtime;
  changing only this adapter's prefix would break focus and keyboard routing.
- A Shell restart re-registers the IME but, like the C++ adapter did, does not
  reconstruct an editor until Android reports another `start` or `state`.
