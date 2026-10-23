# deckard-ime

Native Rust IME adapter for the current DeckardIme Android app and Marathon
Shell `org.marathonos.Shell.TextInput1` contract. It is part of the root Cargo
workspace but does not change the production `deckardd` service.

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

## Cutover gaps

- This binary has not been packaged, deployed, or tested with the real Android
  DeckardIme and Marathon keyboard. Do not run it alongside `deckardd`: both
  would contend for the same socket and Shell registration.
- Shell's service permission manifest still names `deckardd`. A production
  split-service cutover must grant this binary `text-input`, or embed this
  adapter inside the future Rust daemon.
- Existing Android windows and desktop entries still use `waydroid.*` app ids
  and a Waydroid data path. Rename those atomically with the broader runtime;
  changing only this adapter's prefix would break focus and keyboard routing.
- A Shell restart re-registers the IME but, like the C++ adapter, does not
  reconstruct an editor until Android reports another `start` or `state`.
- The rest of the C++ daemon's channels, lifecycle and Python runtime are not
  replaced by this crate.
