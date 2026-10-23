# deckard-bridge (not yet selected)

The Rust user-session replacement for C++ `deckardd`. It is built and tested
but no unit starts it: C++ `deckardd` remains the live bridge until this one
reaches parity. Both claim `org.marathonos.Deckard` and the same sockets, so
they must never run together.

Wired into the daemon (`main.rs`):

- The `org.marathonos.Deckard1` Settings API, with caller authorization and
  the authenticated `system.sock` Manage channel (generation-aware).
- The authenticated app and IME channels, using `deckard-core`'s
  packages.list UID check, bounded length-prefixed JSON decoder, rate limiter
  and bounded writer.
- IME focus and panel replay to the Shell, packages (app and system routing,
  mutation policy) and notifications (Hub listener, persisted state, actions,
  icons, toasts).

Present as modules but **not wired** into the daemon: media/MPRIS and audio
(`mpris.rs`, `audio.rs`), intents/file chooser/capture (`intents.rs`) and
sharing (`sharing.rs`). Their tests include the sources with `#[path]`.

Not ported yet from C++ `deckardd`: Keel and Pilot app providers, permission
prompts, location, shared storage (ACLs, media indexing), Doze, PSI/cgroup
reclamation, palette sync and the boot watchdog. Runtime unit state is polled
once per second rather than subscribed to systemd property changes. The app
channel's incoming event queue is unbounded.

Run the private-bus tests with:

```sh
dbus-run-session -- env DECKARD_BRIDGE_PRIVATE_BUS=1 cargo test -p deckard-bridge --frozen
```
