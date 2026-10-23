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
`/usr/libexec/deckard-launch` and both processes belong to the graphical
user manager. The Pi's `/usr` hierarchy was initially UID 1000-owned; the
session cutover corrected the parent ownership before installing it. This policy passed a
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

An *immediate cold launch* initially failed despite the launcher returning
success and Android reporting Feeder top-resumed: Shell briefly logged the
Feeder window then returned to home. This reproduced with one launcher and
with two concurrent launchers. Calling the old `ShowAndroidApp` both before
and after Binder did not help. The Shell was changed to claim the Android task
before Binder and retain it through a bounded first-window remap. With Shell
PID 112020 held stable, two subsequent stop/start cycles launched Feeder
immediately after starting the Rust session, kept it foreground at +3 and
+11 seconds, and showed Feeder in a device screenshot. The launch CLI calls
took 10.6 and 10.8 seconds, including Android boot; these are not tap-to-first-
frame measurements. The Rust session stopped LXC each time and the legacy
service was restored active with LXC running. A separate test with Shell
restarts during the launch window still reported empty foreground, so Shell
restart overlap remains unqualified. Neither result activates a Rust daemon or
replaces the production Python container/session units.

Five sequential, read-only `sys.boot_completed` property requests within one
SSH/user session took 43.9 ms through `deckard-launch` and 1,351.3 ms through
the installed Python `waydroid prop get`. This is a small warm CLI-overhead
sample (about 8.8 ms versus 270.3 ms per invocation), not a cold-start, app
launch, or end-to-end UI benchmark.

## Pi user-session cutover (2026-10-23)

Installed dynamic-musl `deckard-runtime` and `deckard-launch` under root-owned
`/usr/libexec` and a new on-demand `deckard-session.service` on the Pi.
Stopped `marathon-waydroid.service`; the new Rust service became active and
owned the existing `id.waydro.Session` bus name. Its catalog export rewrote all
eight Android launchers to use `/usr/libexec/deckard-launch` while retaining
their `waydroid.*` desktop IDs and icons. A warm Feeder launch returned success,
Shell reported `waydroid.com.nononsenseapps.feeder` foreground, Android reported
Feeder top-resumed, and a device screenshot showed the app. Stopping the Rust
unit stopped LXC; launching Feeder from that state restarted the unit and
container in 10.91 seconds and restored the visible app. The app-settings
intent opened the Android Settings page; a relay-package handoff error was
fixed and the action then exited 0.

The first Shell-reset trial exposed two activation details. Shell's desktop
parser reduces an absolute `Exec` path to its basename, so the package also
provides `/usr/bin/deckard-launch` as a symlink to the trusted
`/usr/libexec/deckard-launch` executable. The old session stayed alive across
the compositor restart while Android's activity service died. The Rust session
now watches the Wayland socket identity and exits with failure after stopping
LXC when it changes; systemd restarts it. A repeated `marathon reset` changed
the session PID, incremented `NRestarts` to 1, and Android returned to
`sys.boot_completed=1` and `sys.user.0.ce_available=true`. The Pi's
user-owned `/usr`, `/usr/bin`, `/usr/lib`, and `/usr/libexec` parent directories
were corrected to root ownership before moving the binaries from the initial
`/opt` staging location to their package-compliant `/usr/libexec` paths.

This is a **user-session and desktop launcher cutover**, not a full Deckard
cutover. `waydroid-container.service` still runs the Python root manager and
`deckardd` remains C++. The Rust session does not yet implement the upstream
user monitor, clipboard bridge, auto-ADB, or timezone propagation. A full
reboot under the packaged unit still requires
qualification. Rollback is to stop `deckard-session.service`, run
`systemctl --global unmask marathon-waydroid.service` as root, start that
legacy user unit, and restore desktop `Exec=` lines to `waydroid`.
