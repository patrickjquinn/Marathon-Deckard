# deckard-mpris

Rust replacement for the removed C++ `deckardd` mpris.cpp. Each Android MediaSession
(`media`/`media_gone` on the app channel) becomes an MPRIS2 player on its own
session-bus connection; controls go back as `media_cmd` messages through the
sink `clear` installs for each Android connection. Art is re-encoded through `deckard_core::png_guard` into a
cache directory the adapter empties at start. The legacy bus name,
`waydroid.<package>` desktop entry, object path and `media_cmd` keys stay until
the coordinated desktop identity migration.

```sh
cargo test -p deckard-mpris --frozen
```

The tests start a private `dbus-daemon`. Device round trips with real Android
sessions and the Shell's media controls are still required before cutover.
