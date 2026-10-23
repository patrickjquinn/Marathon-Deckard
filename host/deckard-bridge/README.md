# deckard-bridge

Marathon's end of the Deckard bridge (`protocol/bridge.md`): the user-session
daemon that owns `org.marathonos.Deckard` on the session bus and the
`bridge.sock`, `system.sock` and `ime.sock` sockets in
`~/.local/share/waydroid/data/deckard/`. It replaces the C++/Qt `deckardd`
with the same bus names, object paths, interfaces, socket messages and file
paths. Only one owner of `org.marathonos.Deckard` can run.

The user unit is `deckard-bridge.service` (`host/packaging`), running
`/usr/libexec/deckard-bridge` for the graphical session. The Shell admits it
for `text-input` and `android-permissions` through
`/usr/share/marathon-shell/services/deckard-bridge.service.json`, whose `exec`
must match the installed binary.

## Where each `deckardd` responsibility lives

| C++ (`host/deckardd/src`, removed) | Rust |
|---|---|
| `main.cpp`: data, cache and state paths, `org.marathonos.Deckard` (do-not-queue), the three sockets, wiring | `src/main.rs` |
| `bridge.{h,cpp}`: `SO_PEERCRED` + `packages.list` UID check, length-prefixed JSON, rate limit, bounded writes | `deckard-core::{transport, framing}`, `src/app_channel.rs` (bounded inbound and outbound queues) |
| `validate.{h,cpp}` | `deckard-core::{validate, messages, ids, text}` |
| `images.{h,cpp}`: bounded decode and re-encode of icons and art | `deckard-core::png_guard` |
| `callers.{h,cpp}`: D-Bus caller to Marathon app id via the Shell | `src/auth.rs` |
| `settingsapi.{h,cpp}`: `org.marathonos.Deckard1` (runtime, per-app settings, special access, Google services, reset), `RuntimeChanged`, `AppChanged` | `src/api.rs` |
| `manage.{h,cpp}`: DeckardSystem request/reply by id, per generation | `src/manage.rs` |
| `runtime.{h,cpp}`: runtime state, start at login, `UnitWatch` on `deckard-session.service` | `src/runtime.rs` (systemd `PropertiesChanged`), `deckard-core::runtime_state` |
| `bootwatchdog.{h,cpp}`: `/data/deckard/booted`, `deckard-boot-evidence`, one restart | `src/watchdog.rs`, `deckard-core::runtime_state::BootWatchdog` |
| `sessionfollower.{h,cpp}`: `TryRestartUnit` when `org.marathonos.Shell` returns | `src/watchdog.rs` `follow_shell` |
| `reclaimer.{h,cpp}`: `memory.events` and PSI triggers, `reclaim`/`reclaimed` | `src/reclaim.rs`, `deckard-core::pressure` |
| `doze.{h,cpp}`: `/data/deckard/doze` from `Power1.StateChanged` | `src/shell_sync.rs` `sync_doze` |
| `palette.{h,cpp}`: `/data/deckard/palette` from the ambience highlight | `src/shell_sync.rs` `sync_palette`, `deckard-core::palette` |
| `packages.{h,cpp}`: `org.marathonos.Deckard1.Packages`, install staging, mutation policy | `deckard-packages`, `src/packages.rs` |
| `notifications.{h,cpp}`: Hub posts, actions, icons, toasts, `notifications.json` | `deckard-notifications`, `src/notifications.rs`, `deckard-core::notification_state` |
| `mpris.{h,cpp}`: one MPRIS2 player per media session, album art | `deckard-mpris`, routed in `main.rs` `serve_app` |
| `audio.{h,cpp}`: `{t:audio,quiet,alarm}` from the Shell's profile, DND and alarm volume | `src/audio.rs` |
| `ime.{h,cpp}`: IME channel, `ShellTextInput` | `deckard-ime`, `src/ime.rs` |
| `intents.{h,cpp}`: `open` routing, FileChooser picks and saves, camera capture, transfers, focus | `src/intents.rs` |
| `share.{h,cpp}`: `org.marathonos.Deckard1.Share`, `share_out` | `src/sharing.rs` |
| `keelapps.{h,cpp}`: `waydroid.<pkg>` Keel Actions providers, `apps.json`, shortcuts, secure windows | `src/keel.rs`, `deckard-core::keel` |
| `permissionprompts.{h,cpp}`, `permissionmap.{h,cpp}` | `src/prompts.rs`, `src/contract.rs` |
| `location.{h,cpp}`: one GeoClue client while Android asks | `src/location.rs` |
| `sharedstorage.{h,cpp}`: `media_rw` ACLs, `media_scan` | `src/storage.rs`, `deckard-core::acl`, `deckard-host-tools::shared_folders::host_folders` |

The C++ tests are carried over as unit tests in those crates and as the
private-bus tests in `tests/`.

## Known differences from `deckardd`

- The FileChooser titles are the untranslated "Open" and "Save".
- A picked host file that is itself a symlink is refused rather than followed.
- The boot watchdog does not log "the restarted session stalled too" or "the
  shell is restarting repeatedly"; the behaviour is unchanged.
- Failing to set up MPRIS art, audio sync or Keel at start stops the daemon
  (systemd restarts it), where `deckardd` carried on without them.

Run the private-bus tests with:

```sh
dbus-run-session -- env DECKARD_BRIDGE_PRIVATE_BUS=1 cargo test -p deckard-bridge --frozen
```
