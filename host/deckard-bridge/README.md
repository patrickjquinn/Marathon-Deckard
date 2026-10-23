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
- The session unit followed through systemd's `PropertiesChanged`, the boot
  watchdog (`/data/deckard/booted`, `deckard-boot-evidence`) and the Shell
  follower (`TryRestartUnit` when `org.marathonos.Shell` returns).
- Memory reclaim from the container's `memory.events` and PSI triggers
  (`reclaim.rs`), and the ambience palette and Doze written to
  `/data/deckard/palette` and `/data/deckard/doze` (`shell_sync.rs`).
- Media sessions as MPRIS players (`deckard-mpris`), cleared whenever the app
  channel connects or drops, and Marathon's audio policy (`audio.rs`) sent on
  connect and on Shell changes.

Present as modules but **not wired** into the daemon: intents/file
chooser/capture (`intents.rs`) and sharing (`sharing.rs`). Their tests include
the sources with `#[path]`.

Not ported yet from C++ `deckardd`: Keel and Pilot app providers, permission
prompts, location and shared storage (ACLs, media indexing). Each app
channel's incoming queue holds one rate-limiter burst; past that, messages are
dropped, and a peer that keeps flooding is disconnected.

Run the private-bus tests with:

```sh
dbus-run-session -- env DECKARD_BRIDGE_PRIVATE_BUS=1 cargo test -p deckard-bridge --frozen
```
