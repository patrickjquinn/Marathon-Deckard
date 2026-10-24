# Deckard benchmarks (HackberryPi, 2026-10-24)

Rust Deckard against what it replaced, on one phone, one image, one boot:

- **Bridge:** the Rust `deckard-bridge` (deployed build of `85b964c`) against
  the C++/Qt `deckardd` from package r11.
- **Root and session side:** Rust `deckard-containerd` + `deckard-runtime`
  against upstream Python Waydroid 1.6.3 (`waydroid container start`,
  `waydroid session start`).

Scripts are in `tools/bench/`; raw output is in `tools/bench/results/`.
Every number below is from those files.

## Results

Medians over runs, with the min–max of the per-run values in brackets.
Memory is from `smaps_rollup` at the end of the window. "Wakeups" are
voluntary context switches per second, summed over the process's threads.

### Bridge (3 runs each, alternating Rust and C++; Android booted)

| Metric | Rust `deckard-bridge` | C++ `deckardd` |
|---|---:|---:|
| PSS | 7,128 KiB (7,118–7,133) | 13,916 KiB (13,909–13,925) |
| USS | 7,116 KiB (7,108–7,124) | 7,916 KiB (7,900–7,932) |
| Private dirty | 2,316 KiB (2,308–2,324) | 6,688 KiB (6,664–6,692) |
| RSS | 7,768 KiB (7,764–7,840) | 64,088 KiB (64,080–64,228) |
| Threads | 38 | 2 |
| Idle CPU, 120 s | 49.8 ms (49.2–51.1) | 0 ms (0–0) |
| Idle wakeups | 30.8/s (30.8–30.8) | 0/s (0–0) |
| Workload CPU (≈95 s window) | 85.4 ms (85.2–86.3) | 103.3 ms (82.7–105.7) |
| Workload wakeups | 39.5/s (38.7–39.5) | 6.8/s (5.5–7.0) |
| MPRIS Seek → Seeked | 103.1 ms (103.0–103.4), 4 runs | 103.5 ms (103.0–103.7), 3 valid runs of 4 |
| Notification post → Hub `Notify` | 3.05 ms (2.88–3.09) | 3.48 ms (3.15–3.54) |
| OSK `CommitString` → Android → `SetSurroundingText` | 11.0 ms (10.5–11.9) | 13.7 ms (12.6–13.7) |
| Start → owns `org.marathonos.Deckard` | 99 ms (93–101) | 152 ms (144–167) |
| Start → all three Android peers connected | 3,530 ms (3,528–3,536) | 3,462 ms (3,443–3,517) |
| Binary (stripped) | 6.0 MB, links only musl and libgcc_s | 0.79 MB + Qt 6 Core/DBus/Gui/Network |

Pooled latency samples (20 per run):

| Probe | Rust median / p90 / max | C++ median / p90 / max |
|---|---|---|
| MPRIS | 103.2 / 104.3 / 119.4 ms (n=80) | 103.3 / 104.5 / 122.0 ms (n=60) |
| Notification | 3.07 / 4.72 / 17.7 ms (n=60) | 3.46 / 4.46 / 16.0 ms (n=60) |
| OSK keystroke | 11.5 / 17.6 / 30.0 ms (n=60) | 13.3 / 19.1 / 29.7 ms (n=60) |

### Root and session side (3 runs each; Rust runs first, then Python)

| Metric | Rust containerd | Python `container start` | Rust runtime | Python `session start` |
|---|---:|---:|---:|---:|
| PSS | 2,668 KiB (2,665–2,669) | 23,285 KiB (23,282–23,305) | 2,489 KiB (2,485–2,496) | 23,146 KiB (23,103–23,161) |
| USS | 2,656 KiB (2,656–2,660) | 20,824 KiB (20,816–20,844) | 2,480 KiB (2,476–2,484) | 20,664 KiB (20,632–20,712) |
| Private dirty | 348 KiB (348–352) | 20,808 KiB (20,804–20,812) | 492 KiB (488–496) | 20,612 KiB (20,612–20,616) |
| RSS | 3,300 KiB (3,292–3,380) | 35,116 KiB (35,028–35,120) | 3,124 KiB (3,116–3,208) | 35,000 KiB (34,984–35,004) |
| Threads | 7 | 7 | 11 | 7 |
| Idle CPU, 120 s | 67.3 ms (66.5–71.3) | 0 ms (0–0) | 37.6 ms (36.5–37.6) | 0 ms (0–0) |
| Idle wakeups | 60.7/s (54.0–69.7) | 0/s (0–0) | 24.0/s (24.0–24.0) | 0/s (0–0) |
| Workload CPU (ten launches, ≈32 s) | 29.8 ms (29.2–30.2) | 21.1 ms (18.1–24.5) | 13.8 ms (13.6–14.0) | 0 ms (0–0) |
| Workload wakeups | 53.3/s (52.2–58.7) | 1.3/s (1.2–1.4) | 24.0/s (24.0–24.0) | 0/s (0–0) |
| Start → owns bus name | 340 ms (327–354) | 257 ms (238–270) | 93 ms (88–99) | 304 ms (289–431) |

| Metric | Rust | Python |
|---|---:|---:|
| Session start → `sys.boot_completed` | 10.71 s (10.69–10.75) | 10.93 s (10.92–10.99) |
| `deckard-launch app launch`, pooled (n=30) | 151 ms median, 174 ms p90 | 151 ms median, 190 ms p90 |
| `deckard-launch app launch`, median of runs | 150 ms (148–160) | 155 ms (145–164) |

## Idle after the polling fixes (deckard r15, 2026-10-24)

Voluntary context switches summed over threads and CPU time, 60 s idle with
Android booted and all three peers connected, packaged r15 on the phone:

| Daemon | Wakeups | CPU | Threads |
|---|---:|---:|---:|
| `deckard-bridge` | 0/s (was 30.8/s) | 0 ms (was 25 ms per 60 s) | 27 (was 38) |
| `deckard-containerd` | 0/s (was 60.7/s) | 0 ms (was 34 ms per 60 s) | 6 |
| `deckard-runtime` | 0/s (was 24.0/s) | 0 ms (was 19 ms per 60 s) | 10 |

The sections below record the original comparison and are kept as measured.

## What the numbers say

- **Memory: Rust wins clearly.** The bridge's private dirty memory is about a
  third of `deckardd`'s, and its PSS half. Each Rust host daemon is under
  3 MB PSS against about 23 MB for each Python process. USS is the fairest
  bridge number (PSS credits `deckardd` with Qt pages it shares with the
  Shell); there Rust leads by 0.8 MB.
- **Idle cost: Rust loses.** Python Waydroid and `deckardd` sleep in their
  event loops and recorded zero CPU and zero wakeups in every idle window.
  All three Rust daemons poll:
  - `deckard-bridge`: each Android session reads with a 100 ms timeout
    (`host/deckard-bridge/src/app_channel.rs:16`, `READ_POLL`); three
    sessions give the 30 wakeups/s measured.
  - `deckard-containerd`: the main loop polls its hardware-request channel
    every 100 ms (`host/deckard-container/src/bin/deckard-containerd.rs:90`);
    the rest of its 55–70/s was not traced.
  - `deckard-runtime`: checks the Wayland socket every 100 ms
    (`host/deckard-runtime/src/lib.rs:328`) and waits for package events with
    a 500 ms timeout (`lib.rs:258`), about 12/s; the rest of its 24/s was
    not traced.

  The CPU involved is small (0.03–0.06 % of one core each), but on a phone
  wakeups cost power, and the old code had none.
- **Under load the Rust bridge used less CPU** than `deckardd` (85 ms against
  103 ms over the same workload, including Rust's idle polling), and its
  keystroke and notification round trips were faster: 11.0 against 13.7 ms
  and 3.05 against 3.48 ms. The per-run medians do not overlap. The
  keystroke round trip includes the Android IME and editor, the notification
  time includes Android's notification service, and both are the same code
  for both bridges.
- **MPRIS is a tie at about 103 ms**, for both bridges. The floor is on the
  Android side of the bridge, not in either host daemon; where it comes from
  was not investigated.
- **Host workload: Python Waydroid used less CPU.** Over ten app launches the
  Python container used 21 ms against containerd's 30 ms; the Python session
  used none, the Rust runtime 14 ms (its polling). Launch times were the
  same.
- **Startup:** the Rust bridge owns its bus name about 50 ms sooner than
  `deckardd`, and the Rust session about 210 ms sooner than Python's. The
  Rust container unit is slower to its bus name (340 against 257 ms) because
  its unit also runs `deckard-overlay-sync` and `deckard-fonts-sync` before
  the daemon: those took 291 ms in one measured start, leaving about 50 ms
  for containerd itself. The Python unit runs no sync step.
- **Android boot** was 0.22 s faster under the Rust stack (10.71 against
  10.93 s) and the ranges do not overlap, but the runs were not interleaved
  (see caveats), so treat this as small and unconfirmed.
- **Peer reconnection** after a bridge start took about 3.5 s for both; it is
  set by the Android apps' reconnect timing.

## Method

Device: HackberryPi, Raspberry Pi CM5 Lite, 4 cores, 16 GB, kernel
6.12.85-0-rpi, governor `schedutil`, image "Nura edge", with the Deckard
binaries the integrator hot-deployed from `85b964c`. The Shell was
running throughout, screen timeout off, Android data from earlier tests.
Caches were not dropped (forbidden on this device), so every run is warm.

Probes (`tools/bench/phone/`, run as root on the phone):

- `sample.sh` reads, per PID, `/proc/PID/task/*/schedstat` (CPU ns,
  timeslices), `/proc/PID/task/*/status` (voluntary and involuntary switches)
  and `/proc/PID/stat` (CPU ticks, as a cross-check) at the start and end of
  a window, then `smaps_rollup` and `Threads`. USS is
  `Private_Clean + Private_Dirty`.
- `bench.py startup` starts a unit and times, from just before the start
  command: the bus name's `NameOwnerChanged` (from `dbus-monitor`), the three
  Android peers (the bridge's sockets that appear in the container's
  `/proc/net/unix`, polled) and `sys.boot_completed` (`getprop` through
  `lxc-attach`, polled about every 70 ms).
- `bench.py latency` times round trips from `dbus-monitor`'s receive
  timestamps on the session bus:
  - MPRIS: `busctl call ... Seek x ±20000000` on the Notify Test app's silent
    media session to the bridge's `Seeked` signal; 20 per run, 1 s apart.
  - Notifications: Notify Test posts a plain notification with a unique text
    (`am broadcast`); the start is its own `DeckardTest: post plain` logcat
    line (`-v epoch,usec`), the end the bridge's `Notify` call carrying that
    text. Android and the host share the kernel clock. 20 per run, each 1 s
    after the previous `am broadcast` returned.
  - Keystrokes: `marathon-touchctl` taps the OSK "a" key over Markor's
    search field; from the Shell's `CommitString` to the bridge's
    `SetSurroundingText` with Android's new text. 20 per run, 0.7 s apart.
- `bench.py launches` runs `deckard-launch app launch` (in the user
  manager, as the desktop entries do) ten times, cycling Notify Test,
  Settings and Markor, 3 s apart.

Runs (`phase.sh`, driven from the host by `bench.sh`, as a transient root unit):

- **Bridges:** for each of 3 rounds, Rust then C++: stop both bridge units,
  wait 3 s, start one (startup), settle 30 s, sample 120 s idle, then sample
  across the three latency probes. Android stayed booted under the Rust
  host stack, with Markor's search field focused and the OSK shown
  (`ime-setup.sh`). `swap.sh cxx-bridge` installed r11's `deckardd`,
  its unit (in `/run/systemd/user`) and its Shell service declaration;
  `swap.sh rust-bridge` removed them.
- **Hosts:** from Android and the container manager stopped: start the
  container unit (startup), start the session (startup, boot), settle 30 s,
  sample both daemons 120 s idle, sample across ten launches, stop both.
  Three Rust runs, then `swap.sh python-host` (Waydroid's own D-Bus
  activation file, from its 1.6.3-r0 package, verified against the phone's
  apk database), three Python runs, then `swap.sh rust-host`. The Python
  session ran as a transient user unit with the same `WAYLAND_DISPLAY` as
  `deckard-session.service`; the Rust bridge served Android in both.

`snapshot.sh` fingerprints the Deckard units, files and state; the
snapshots before and after these runs were identical.

To repeat: boot Android under the Rust stack, run `phone/ime-setup.sh` on
the phone and check a screenshot, then `tools/bench/bench.sh bridges`; then
`tools/bench/bench.sh hosts`. `summarize.py` prints the tables.

## Caveats

- One device, one boot, warm caches, 3 runs per stack (4 for MPRIS). The
  bridge runs alternate; the host runs do not (Rust first, then Python), so
  drift between the two halves is not controlled.
- Only each daemon's main process is measured. Both container units also run
  `dnsmasq`; both use the same LXC monitor. The Python processes had no other
  children (each run lists its unit's processes in `hosts.txt`).
- Switch counts come from live threads only; a thread that exits inside a
  window loses its count. Thread counts did not change in any window.
- `dbus-monitor`'s receive time adds the same scheduling jitter to both ends
  of a round trip. Logcat times have microsecond resolution but are stamped
  inside Android's logger.
- The workloads exercise only what is listed: no package installs, sharing,
  Keel, permission prompts or file transfers.
- Run `cxx-3`'s MPRIS probe failed all 20 seeks with "The name is not
  activatable": `deckardd`'s player name was listed, then vanished. A fourth
  C++ run (`bridges-mpris-rerun.txt`) did not reproduce it; the table uses
  the three valid C++ runs and four Rust runs.
- During a trial launch before the measured runs, launching Settings while
  Markor showed a dialog with the IME made Android's system_server watchdog
  restart it (WindowManagerService blocked on the display and animation
  threads). It did not recur in the 60 measured launches.
