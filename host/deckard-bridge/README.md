# deckard-bridge (inactive)

This standalone user-session binary implements the `org.marathonos.Deckard1`
Settings interface and the authenticated Android `system.sock` Manage channel.
It is a migration slice, not a device replacement: no service, packaging, or
launcher invokes it. It must not run alongside C++ `deckardd`, because both
claim `org.marathonos.Deckard` and use the same system socket.

The socket uses `deckard-core`'s packages.list UID check, bounded length-prefixed
JSON decoder, rate limiter, and writer. The daemon currently does not serve the
other Deckard interfaces or the app and IME sockets. Runtime unit state is
polled once per second rather than subscribed to systemd property changes.

Run the slice's private-bus contract tests with:

```sh
dbus-run-session -- env DECKARD_BRIDGE_PRIVATE_BUS=1 cargo test -p deckard-bridge --test contract --locked
```
