# Rust cutover baseline (Hackberry Pi, 2026-10-23)

This records the production contract before replacing either running service.
Measurements are a single warm snapshot, not cold-boot or peak values.

| Process | Unit | Threads | RSS |
|---|---|---:|---:|
| `/usr/bin/deckardd` (Qt/C++) | `deckardd.service` | 2 | 62,876 KiB |
| `python3 /usr/bin/waydroid container start` | `waydroid-container.service` | 7 | 34,944 KiB |
| `/usr/bin/python3 /usr/libexec/deckard-waydroid session start` | `marathon-waydroid.service` | 6 | 34,528 KiB |

The master plan's 208-thread and 4-5x cold-start figures are design targets,
not observations on this device. Re-measure after a complete cutover before
claiming a performance improvement.

## Live contracts

- The C++ daemon owns `org.marathonos.Deckard` at `/org/marathonos/Deckard`.
  Its session-bus interfaces are `org.marathonos.Deckard1`,
  `org.marathonos.Deckard1.Packages`, and `org.marathonos.Deckard1.Share`.
  The three Android sockets are `bridge.sock`, `system.sock`, and `ime.sock`.
- The root Python service owns `id.waydro.Container` on the system bus. The
  graphical Python session owns `id.waydro.Session` on the session bus. Android
  app desktop entries still execute the Python `waydroid app launch` CLI.
- The installed Android window/desktop IDs are `waydroid.<package>` and Store
  IDs are `android:<package>`. The new `deckard.<package>` ID type in Rust is
  not yet the production wire identity.
- `/var/lib/waydroid` is about 4.2 GB and the graphical user's Waydroid data
  tree is about 154 MB. Both are on the same ext4 data device (ID 65024). The
  data device had about 5.9 GB free; do not duplicate the whole image tree for
  migration. Stop and verify all mounts before a same-filesystem rename.

## Cutover gate

Do not select a Rust `deckardd` or rename the active units until native code
handles all three sockets, D-Bus interfaces and authorization, notifications,
MPRIS, IME, Settings/packages, intents/sharing, runtime, and Keel contracts.
The Rust user/root control plane must also preserve app launch, desktop export,
unlock/package monitors, binder hardware operations, mounts, networking, and
clean shutdown. Run upgrade and rollback on the physical device with the
existing app data intact, then trace boot and app launch for Python execution.

Target unit names are `deckard-session.service` and
`deckard-container.service`. The current names remain until the native
implementations and state migration pass this gate.

The prototype container authorization permits a separate launcher for
`GetSession`/`Unfreeze` only when its executable is the root-owned
`/opt/deckard/bin/deckard-launch` and both processes belong to the graphical
user manager. The Pi's `/usr/bin` is owned by UID 1000, so installing the
launcher there would not meet this trust policy. This policy passed a
private-bus fixture, not a live replacement of the root container service.

## Native launcher smoke test

On the same Pi, an AArch64 dynamic-musl `deckard-launch` candidate ran as the
graphical user against the live `libgbinder.so.1` and session/system buses.
`prop get sys.boot_completed` returned `1`. `app launch
com.nononsenseapps.feeder` returned successfully and Android `dumpsys activity`
reported Feeder's `MainActivity` as top-resumed and visible. An app-settings
intent also completed without a Binder transport error.

The Shell screenshot still showed Shortwave over Feeder after launch. Android
activity success is therefore insufficient as a user-visible launch test; the
Shell must raise the Android window before switching generated desktop entries
to this binary. No live unit or desktop entry was changed for this test.

## Candidate session and foreground cycle

The Shell now exposes `ShowAndroidApp` for a Binder launcher to request an
Android window handoff. A dynamic-musl `deckard-runtime` candidate was run as
the graphical user under a transient user unit against the *existing Python
container manager*. The correct display is `marathon-wayland-0`. After the
legacy user session was stopped, the candidate acquired `id.waydro.Session`,
started LXC, launched Feeder through `deckard-launch`, and Shell moved from
Shortwave to `waydroid.com.nononsenseapps.feeder`; Android reported Feeder
top-resumed. `deckard-runtime session stop` released the bus name and left LXC
`STOPPED`. A second candidate session restarted LXC and repeated the visible
launch. The legacy `marathon-waydroid.service` was then restored and confirmed
active with LXC running.

An *immediate cold launch* during a subsequent candidate session start is not
yet reliable. The launcher returned success and Android reported Feeder
top-resumed, but Shell briefly logged the Feeder window then returned to home
(`GetForegroundApp` returned empty). This reproduced with one launcher and
with two concurrent launchers. Asking the Shell to `LaunchApp` later restored
Feeder, so the candidate still has a cold-window lifecycle/handoff gap. Merely
calling `ShowAndroidApp` before as well as after the Binder launch did not
resolve it. Do not select the candidate session or launcher for production
until the immediate cold path stays visible under repeated device tests.

Five sequential, read-only `sys.boot_completed` property requests within one
SSH/user session took 43.9 ms through `deckard-launch` and 1,351.3 ms through
the installed Python `waydroid prop get`. This is a small warm CLI-overhead
sample (about 8.8 ms versus 270.3 ms per invocation), not a cold-start, app
launch, or end-to-end UI benchmark.
