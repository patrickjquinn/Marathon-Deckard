# System bridge

How Android notifications, media sessions, sound and back reach Marathon
(spec §10). Measured on the HackberryPi CM5 with Waydroid 1.6.3, LineageOS 20
VANILLA (20260927, userdebug, test-keys) and marathon-shell r479, on
2026-10-18. The wire protocol is in [../protocol/bridge.md](../protocol/bridge.md).

## Shape

```
Android app ──notify()──▶ NotificationManagerService
                              │ NotificationListenerService
                              ▼
                    DeckardBridge (priv-app, uid 10126)
                              │ /data/deckard/bridge.sock
                              │ (= ~/.local/share/waydroid/data/deckard/bridge.sock)
                              ▼
                    deckardd (host, user 1000)
                ┌─────────────┼─────────────────────┐
     org.freedesktop.     org.mpris.MediaPlayer2.   org.marathonos.Shell
     Notifications (Hub)  waydroid_<pkg>.instanceN  Settings1/Audio1 (sound state)
```

- **DeckardBridge** (`image/apps/DeckardBridge`, Java, about 750 lines,
  30 KB APK) is a NotificationListenerService that also tracks MediaSessions
  through the listener's `getActiveSessions` grant. Android starts it at boot
  (`android:persistent`, priv-app), and binds it as a listener once the init
  rc has run `cmd notification allow_listener`.
- **deckard-bridge** (`host/deckard-bridge`, Rust, Apache-2.0; it replaced the
  C++/Qt deckardd) owns the socket, the
  Hub client, the MPRIS players and the audio sync. The keyboard agent's IME
  channel runs on a second socket in the same daemon.

## Transport

A Unix socket in Android's `/data`, which Waydroid bind-mounts from the user's
`~/.local/share/waydroid/data`. So no new LXC mount entry, no TCP, and no
`waydroid0` address is needed. A pathname socket works across the container's
network namespace.

- **SELinux:** `getenforce` in the container says `Disabled` (the host kernel
  has no SELinux), so no policy is involved.
- **Authentication:** deckardd checks `SO_PEERCRED` on every connection. The
  peer's uid must be the uid `packages.list` gives
  `org.marathonos.deckard.bridge` (10126 on this device). There is no user
  namespace, so Android uids are host uids. A connection from any other uid
  is closed at once (`tests/test_bridge.cpp`).
- **Limits:** frames are 1 MiB at most. The rate limit is a 200-message burst,
  then 50 per second; after 1,000 drops the connection is closed. At most 200
  notifications and 8 players are tracked. Every field is bounded and cleaned
  (`validate.cpp`, `tests/test_validate.cpp`). Every image is decoded,
  bounded to 4096 x 4096, scaled and re-encoded on the host.
- **Reconnects:** the bridge retries with backoff (0.5 s doubling to 10 s). On
  every connect it re-sends all active notifications, then a `sync` with their
  keys. deckardd keeps key → Hub id in `~/.local/state/deckard/notifications.json`,
  so a restart of either side neither duplicates nor orphans Hub rows.

## Notifications in the Hub

Checked on the device:

| What | Result |
|---|---|
| Name and icon | `app_name` is the Android label ("Download Manager", "Notify Test"). The app's icon is exported to `~/.cache/deckard/icons/<pkg>.png` and passed as `app_icon` (`file://`). Download Manager showed its real icon. The large icon travels as `image-data`. The Hub shows the app as "Waydroid.<package>": r479's registry doesn't know the `waydroid.<pkg>` desktop entries (see *What the shell must change*). |
| Grouping | `desktop-entry` = `waydroid.<pkg>`. The Hub's filter chips group by it ("Waydroid.com.android.providers.downloads 2"). |
| Body | HTML-escaped: "Two actions & a <tag>" rendered literally. |
| Actions | Tapping "First" in the Hub logged `event first` in the app. The Hub then closed the row (not resident), and because the notification auto-cancels, deckardd cancelled it on Android (0 left in `dumpsys notification`). |
| Inline reply | `inline-reply` action plus `x-kde-reply-placeholder-text` ("Message Ann"). `NotificationReplied(26, "See you at 9, bringing rope")` reached the app's `RemoteInput` (`event reply reply="See you at 9, bringing rope"`). The app re-posted with the reply appended, and the Hub row was replaced (id 26 → 27). The reply was sent through the Hub's own `InvokeReply`: the Hub list has no reply field, only the banner does (see *What the shell must change*). |
| MessagingStyle | `x-marathon-conversation` carries `{title, self, group, messages[]}` (the last 10). The Hub doesn't render it yet. |
| Progress | `value` = percent. The Hub's ongoing row showed a bar at 50 % that moved in place each second, then "Done" when the app finished (ongoing cleared). |
| Removal, Android → Hub | `cancelAll()` in the app removed the row from the Hub at once (`CloseNotification`). |
| Removal, Hub → Android | Swiping a row away (reason 2) cancelled it on Android, for the test app and for Download Manager ("Download complete.", "Download unsuccessful."). |
| Tap | Sends the `contentIntent` (the bridge holds `START_ACTIVITIES_FROM_BACKGROUND`) and cancels it on Android if it auto-cancels. The Hub can report the close of a tapped row before the tap, so a closed row stays matchable for 500 ms (3dc5f4c). The first tap test, before that fix, lost the tap. |
| Real system app | A DownloadManager download (12.6 MB over HTTPS, done in 2.6 s) produced Download Manager's "Download complete." in the Hub. The failed cleartext attempt before it produced "Download unsuccessful.". The download was too short for DownloadProvider to post an in-progress notification. |
| Latency | From the app's `notify()` to the Hub's `Notify` call, 10 posts: **median 4 ms** (3–5 ms). With Android's notification assistant enabled it was 205 ms (204–209 ms): NotificationManagerService holds every post for `DELAY_FOR_ASSISTANT_TIME` (200 ms). The rc turns the assistant off. |
| Not forwarded | MediaStyle notifications (they are MPRIS players), group summaries that have children, the bridge's own notifications, and `android` package noise (`PHYSICAL_KEYBOARD`, `DEVELOPER`, `DEVELOPER_IMPORTANT`, `USB`, `FOREGROUND_SERVICE`). The "Configure physical keyboard" rows still in the Hub date from Waydroid's own forwarder, before Deckard. |
| Unchanged updates | Not re-posted: deckardd compares a digest of everything the Hub shows. That digest leaves out `suppress-sound`, because Android marks repeats `FLAG_ONLY_ALERT_ONCE` (1bf05f8). |

Android draws nothing: `heads_up_notifications_enabled` is 0, and Waydroid's own
forwarder (`notification_manager.py`, which posted every notification a second
time and has no replies, removal from the Hub or progress) does not run: the
Rust session (`deckard-runtime`) has no forwarder of its own.

## Media sessions as MPRIS players

Checked with the test app's media session, which plays nothing (no audible
tests tonight):

- `org.mpris.MediaPlayer2.waydroid_org_marathonos_deckard_notifytest.instance1`
  appeared, owned by deckardd on its own connection, with `Identity` "Notify
  Test", `PlaybackStatus` "Playing", and `Metadata` (`xesam:title`,
  `xesam:artist`, `xesam:album`, `mpris:length` 200000000, `mpris:trackid`,
  `mpris:artUrl` file:///home/user/.cache/deckard/art/1-1.png). `Position`
  advanced on its own (3308000 µs after 3 s).
- Over D-Bus, Pause, Next, `SetPosition(track2, 60 s)` and PlayPause reached
  the session as `media pause`, `media next`, `media seek 60000` and
  `media play`. Afterwards Position read 60505000 and PlaybackStatus Playing.
- Marathon's Quick Settings media card showed the session (art, "Silent track
  2", "Deckard"). Its pause and next buttons reached Android, and the card
  switched to track 3 and its new art.
- Releasing the session removed the player from the bus and deleted its art.

## Sound: Android follows Marathon

Android played its own notification, touch and alarm sounds straight into
PulseAudio, whatever Marathon's profile was. On the night of the 18th that was
a chime through a Bluetooth speaker while Marathon was silent. Now:

- deckardd reads `audioProfile`, `dndEnabled` and `alarmVolume` from
  `org.marathonos.Shell.Settings1.GetState`, re-reads them on
  `Audio1.StateChanged`, `Settings1.PropertyChanged` and a shell restart, and
  sends `{t: audio, quiet, alarm}`.
- The bridge keeps Android's notification, ring and system streams at 0
  always. The Hub plays Marathon's sound, which follows the profile, and
  `suppress-sound` for Android's silent notifications (d23326c2 in the shell).
  Music is at maximum, under Marathon's output volume. The alarm stream is
  Marathon's alarm volume. While Marathon is silent, vibrating or in DND, the
  bridge sets alarms-only Do Not Disturb (`requestInterruptionFilter`); this
  matches Marathon, whose own alarms ring through all three.
- On the device (Marathon in DND): `set_zen_mode: alarms,listener:org.marathonos.deckard.bridge`,
  ringer mode SILENT. The first version set DND before the volumes, and
  zeroing the ring stream turned DND off 16 ms later
  (`set_zen_mode: off,ringerModeInternal`); the order is fixed (eb5a512).
- Touch sounds and haptics are off (`sound_effects_enabled`,
  `haptic_feedback_enabled`).
- PipeWire: Waydroid's audio HAL opens ALSA's `pulse` device with no
  `media.role`, so its stream goes to the default sink, which on Marathon is
  `input.loopback.sink.role.multimedia`. No `PULSE_PROP` override is needed.
  Not observed live: no Android audio was played.

## Palette follows the ambience

Android's 65 `system_accent*`/`system_neutral*` colours track Marathon's
ambience highlight at runtime. DeckardPalette (the chrome agent's static RRO)
stays the boot default.

- **deckardd** reads `/desktop/jolla/theme/color/highlight` from
  `Ambience1.GetAmbience`, again on `AmbienceChanged` and on a shell restart. It
  generates the colours with `palette.cpp`, a port of gen-palette.py: all 65
  are identical to DeckardPalette for #7df0d4 (`test_palette`, which also
  drives a fake Ambience1 on a private dbus-daemon). It writes them to
  `/data/deckard/palette` when they change.
- **Android:** `deckard-watch` (inotifyd on `/data/deckard`, user system) runs
  `deckard-palette.jar` under `app_process`. One overlay-manager transaction
  registers `android:deckard_ambience`, a fabricated overlay holding all 65
  colours, and enables it, which also makes it the highest priority (above
  DeckardPalette at 3; it shows as 2147483647). This is allowed because AOSP 13
  only blocks shell from fabricating, SELinux (disabled here) is what blocks
  apps, and the system uid is an allowed actor. `cmd overlay fabricate` would
  need 65 overlays and 65 configuration changes; this is one.
- **On the device:**
  - Writing a palette for #ff8a3d turned Clock's FAB, selected tab, dialog
    buttons and icons orange.
  - Changing the wallpaper from Meridian to Indigo Dusk gave highlight
    #84e8da. deckardd rewrote the palette and Android resolved
    `system_accent1_100` to #ff84e8da.
  - Contour brought #7df0d4 back through the watcher, live, without a restart
    (the app_process run started 4 s after the tap). Meridian is restored.
  - Every built-in wallpaper derives a teal highlight, so on stock ambiences
    the change is subtle.
- **Found on the way:** toybox inotifyd reports a file renamed into a watched
  directory as `y`, not `m` (deckardd's atomic write is temp file plus rename).
  Also, a configuration change from an overlay relaunches activities; Waydroid
  then recreates the task's window, and Clock's card closed once (shell r479).
- **Open:**
  - Light ambiences (`darkonlight`): the pinned roles and the chrome agent's
    `cmd uimode night yes` assume dark.
  - Apps already running pick up the new colours through the configuration
    change; apps with their own palettes don't change.

## Installing Android apps the Marathon way

Android's installer never shows. deckardd's `org.marathonos.Deckard1.Packages`
(protocol/bridge.md) stages the APK in `/data/deckard/install/`, and
DeckardBridge, which holds INSTALL_PACKAGES and DELETE_PACKAGES through its
privapp file, installs it through a PackageInstaller session. Only Settings,
the Store (Callers::require) and the Store's handler (by executable,
`/usr/bin/marathon-store-handler`) may install or remove apps.

- **From F-Droid in the Store** (HackberryPi, shell b0e7a4db, deckard ec1f9a6,
  2026-10-19):
  - Search for "bouncy" or "sleep timer" listed F-Droid apps beside Flathub's,
    with F-Droid icons.
  - Page (com.page.bizzle, 21 KB) showed "Android app · from F-Droid",
    version, "It asks for no special permissions" and its description.
  - Get → Open in about 1 s. `installerPackageName=org.marathonos.deckard.bridge`,
    and Waydroid wrote `waydroid.com.page.bizzle.desktop`, which the shell's
    registry listed (`apps.list`). Open started the app in its Marathon
    window.
  - Sleep Timer installed the same way but has no launcher activity, so it
    gets no grid entry. The Store still offers Open for such an app; that
    still needs fixing.
- **From a file:** `deckard-open-apk ~/Downloads/red-screen.apk` opened the
  Store's install page with the app's icon, "Red Screen", "Android app · from a
  file", "Version 1.2" and "It can: Draw over other apps". Install → Open,
  with "Red Screen installed" as the Store's notice.
  - Opening the file again said "Update app, Version 1.2, replacing 1.2". The
    button was wrong there (Open, from the earlier job); fixed in Marathon-Shell
    94032a45.
  - The update ran through the handler (`lastUpdateTime` 02:47:15 → 02:48:21).
  - Uninstalling removed the package and its desktop entry.
  - That worked only because DeckardBridge had installed the app. For any
    other app Android answered `STATUS_PENDING_USER_ACTION` (seen by the
    Settings agent after a reset): `deletePackageVersioned` removes silently
    only for the installer of record, the verifier, the uninstaller app or
    the system uid, DELETE_PACKAGES or not. Removals now go to DeckardSystem,
    which runs as the system uid. On the device an app installed with
    `pm install` (no installer of record) was removed through
    `marathon-store-handler`'s uninstall with no prompt.
- **F-Droid catalogue on the device:** a full sync (verify entry.jar,
  download 16 MB, stream-parse 66 MB) took **4.4 s at 59 MB peak RSS**; with
  the index unchanged, 0.8 s. The catalogue holds 4,713 apps in 7.2 MB.

### Will it run here

Before an install the Store says what an Android app needs that this phone
may not give it. From an .apk, DeckardBridge reads the required
`uses-feature`s (`reqFeatures`), the required `uses-library`s (its own
`AndroidManifest.xml` through `XmlResourceParser`), Play services
(`com.google.android.gms.version` meta-data or a `com.google.android.gms`
library) and the native ABIs (`lib/<abi>/` in the zip); from F-Droid the
handler keeps each version's `features` and anti-features. The phone's side
comes from Android itself (`Packages.DeviceProfile`: its features, shared
libraries, ABIs, SDK, whether Play services are installed), so it is right
for each device. Only what can't run blocks the install: no code for the
phone's processor, a newer Android than this one, a required library that
isn't there (Android would refuse it). Everything else is a plain warning
on the app's page: "Needs Google Play services, so it may not work", "Needs a
phone radio", "Needs a camera", and so on. The F-Droid catalogue already
leaves out versions without arm64 code or for a newer Android.

## Memory budget

The LXC start-host hook `deckard-container-limits` set the container's
`memory.high` to 4.80 GiB (5,034,213,376) and `memory.max` to 6.25 GiB on the
16 GB HackberryPi; before it, both were `max`. After boot the container used
1.04 GB (with the slim agent's S6).

Waydroid regenerates `/var/lib/waydroid/lxc/waydroid/config` from
`/usr/lib/waydroid/data/configs/config_base` on `waydroid init` and
`waydroid upgrade` (`tools/helpers/lxc.py` `set_lxc_config`), so the hook only
lasts if it is in `config_base`, which is where pipeline patch 0045 puts it.
On this device it was appended to the live config by hand; the original is
`/var/tmp/lxc-config.pre-deckard-hook`.

Squeezing it:

| memory.high | Result after 15–20 s |
|---|---|
| current + 120 MB | nothing reclaimed; 7 apps started, usage 1.09 GB |
| 900 MB | page cache reclaimed to 933 MB, `high` events 9, PSI 15 ms |
| 700 MB | anon pushed to swap (anon 12 MB, file 459 MB, 556 MB total), `high` events 74, container PSI some 0.68 % |

**Android's lmkd never killed anything:** it logs `/proc/zoneinfo parse
error` / `Failed to get free memory!` every few milliseconds. LineageOS 20's
lmkd can't parse the 6.12 kernel's zoneinfo, in both its minfree and its PSI
paths, so it is effectively off. What protects Marathon is therefore the
kernel:
- reclaim and swap under `memory.high`;
- at `memory.max`, the cgroup OOM killer, which only considers processes in
  the container and prefers the highest `oom_score_adj`. That means cached
  Android apps (900+) first and system_server (−800) and SurfaceFlinger
  (−1000) last.

Fixing lmkd needs a rebuilt lmkd (zoneinfo parser), not a setting. lmkd
cannot simply be stopped: it is what writes each process's `oom_score_adj`
for ActivityManager (`cmd_procprio`), which the cgroup OOM killer relies on,
and it is a `critical` service. So it now runs without pressure monitors
(`ro.lmk.use_new_strategy=true` with both PSI stall thresholds 0, in
`image/waydroid.prop`). On the device it logs "Using psi monitors" and then
nothing, and still sets the adj: cached calendar 915, settings 955, the
foreground app 0.

The killing moved to Marathon. deckardd's `Reclaimer` asks DeckardSystem to
end the two least recently used cached app processes (protocol/bridge.md,
`reclaim`) when the container goes over `memory.high` or `memory.max`
(`memory.events`, watched with inotify), when Android stalls on memory (a
PSI trigger on the container's `memory.pressure`; the whole container cgroup
already belongs to the session's user) or when the whole phone stalls at the
shell's High level (`/proc/pressure/memory`; the shell does not publish its
level on the bus). Idle, it costs nothing: no polling, only kernel
notifications.

Measured on the HackberryPi:
- Lowering `memory.high` alone (1.04 GB in use, to 800 MB, then 500 MB)
  reclaimed page cache on the write itself: no `high` events, no pressure.
- `memory.high` 600 MB with 450 MB allocated inside the container: 245
  `high` events, container PSI some avg10 15 %. Within 2 s DeckardSystem
  ended `android.process.acore` (adj 965) and `android.process.media` (925),
  2 s later `com.android.providers.calendar` (915) and
  `com.android.permissioncontroller` (905): oldest first, two per request.
  Then it stopped: the cached processes left were keychain, shell and
  settings, which run with system uids and are never candidates. The
  foreground app, the persistent apps and system_server were untouched; no
  OOM kill.
- With 2 s windows the phone-wide trigger (20 % of 2 s) fired on each
  Android boot with 10 GB free and ended packageinstaller and
  externalstorage. Both triggers now use 10 s windows (container 10 %, phone
  20 %, the latter the shell's avg10 High level); that change is not yet
  measured.

## Back

The pill's back gesture is a Wayland key, and that works: no bridge
transport is needed.

- **Path.** For a window without `marathon_surface_v1`,
  `WaylandCompositor::sendNativeBack` ends in `sendKeyTo(Qt::Key_Back)`.
  Qt Wayland turns `Qt::Key_Back` into `XF86Back`'s keycode minus 8, which is
  evdev `KEY_BACK` (158). Waydroid's hwcomposer writes it unchanged into
  `/dev/input/wl_keyboard_events`. Android reads that pipe with `Generic.kl`,
  where `key 158 BACK` gives `AKEYCODE_BACK`. Android delivers it to its
  focused window, and Android's focus follows `wl_keyboard.enter` on the
  task's surface (`setFocusedTask`). The shell gives an Android window
  keyboard focus once it's in front and its configure is acked (998bff84),
  and sends back to the front Android window (5ce3cf76).
- **On the device (shell at 6f89be4b+, 2026-10-19 00:13):**

  | App | Gesture | Result |
  |---|---|---|
  | Clock | back with the overflow menu open | menu closed, Clock stayed |
  | Clock | back from Settings (SettingsActivity) | back to DeskClock (topResumedActivity) |
  | Notify Test | back on its root activity | the activity logged `KEYCODE_BACK down` / `up`; Android moved the task back (Android 12+ root back) and Marathon showed Home |
  | Settings | back from Connected devices (SubSettings) | back to Settings |

  On r479 (18 Oct), before the shell's focus and black-card fixes, the
  same gesture went nowhere. Either Android had no focused window
  (`dumpsys input` showed `FocusedWindows:` empty), or the window was black
  and the shell's unhandled-back watch went Home.
- **Not needed:** a bridge `Back(appId)`. An app can't inject keys
  (`INJECT_EVENTS` is signature-only), and an accessibility
  `GLOBAL_ACTION_BACK` also goes to Android's focused window.
- **Still needs the spec's forked hwcomposer:** back with progress
  (`marathon_surface_v1` back events).

## Toasts

Waydroid's multi-window mode drops every layer without a task except the IME
(`hwcomposer/modes/multi-window.cpp`), so toasts never reached the screen.
`ToastPresenter` announces each toast to accessibility, so DeckardBridge has
an accessibility service, `ToastWatcher`, subscribed only to
`TYPE_NOTIFICATION_STATE_CHANGED` and filtered on class
`android.widget.Toast`. Apps only build accessibility events of the types an
enabled service asks for, so this is the only event type they generate.
`deckard-toasts` appends the service to `enabled_accessibility_services` at
boot, keeping any others.

deckardd posts the toast with summary = the text, `app_name` = the app label,
the app's icon, `urgency` 0, `transient`, `suppress-sound`,
`x-marathon-toast` and a 3.5 s timeout, and closes it after 4 s; at most one
per app per 500 ms. The shell (6f89be4b+) draws `x-marathon-toast` as a
Marathon app toast and keeps transient notifications out of the Hub.

- **On the device:** `Toast.makeText(..., "Saved to Downloads")` from Notify
  Test showed as a Marathon toast with the app's icon at the bottom of the
  screen, with no Hub row and no badge. 10 ms from `Toast.show()` to the Hub's
  `Notify` call.

Slim stage S3 removed SystemUI, and with it `ToastPresenter`: text toasts of
apps targeting Android 11 or later go from `NotificationManagerService` to
`IStatusBar.showToast`, which SystemUI implemented, so none were drawn and
ToastWatcher saw nothing (verification-2026-10-19.md, F3). DeckardSystem
now registers its own `IStatusBar` with `StatusBarManagerService`
(`ToastBar`, through reflection on the hidden AIDL classes; it reads the
`showToast` transaction code from `IStatusBar$Stub`). It answers only
`showToast`, handing the package and text to DeckardBridge
(`Handoff.call("toast")`), which sends them on as ToastWatcher's toasts; the
other calls are oneway notices for a status bar and are dropped, as they were
with no `IStatusBar` at all. What changes with one registered:
`requestAddTile` used to answer "no status bar service" at once and now gets
no answer, so an app asking to add a quick settings tile waits; Android's
quick settings don't exist here. Apps targeting older versions still draw
their toasts themselves, and ToastWatcher still reports those.

## Android's freezer and Doze

- **Android's cached-app freezer can't run in this container.**
  `CachedAppOptimizer` takes `use_freezer` from DeviceConfig (the persist
  property does nothing). Even after `device_config put
  activity_manager_native_boot use_freezer true` and a restart it logs
  `unable to read freezer info` / `Freezer disabled`. `isFreezerSupported()`
  calls `BINDER_GET_FROZEN_INFO` with system_server's pid, and the binder
  driver matches pids in the host pid namespace, while the LXC container has
  its own (`pid:[4026532784]` against the host's `pid:[4026531836]`).
  `BINDER_FREEZE` looks processes up the same way. The kernel has the
  feature (binderfs `features/freeze_notification` is 1, and
  `binder_add_freeze_work` exists); the namespace is the obstacle. Android's
  init must be pid 1, so the container can't share the host's pid namespace.
  The fix would be a binder driver that translates pids from the caller's
  namespace (kernel work). Freezing app cgroups from the host without binder
  freeze would hang their binder peers. The DeviceConfig value is left true;
  it has no effect.
- **Doze → deviceidle.** deckardd writes `/data/deckard/doze` (1 or 0) when
  the shell's `Power1.StateChanged` carries `dozing`. The shell doesn't
  publish Doze yet (queued with the shell agent). `deckard-watch` then runs
  `cmd deviceidle force-idle deep` or `unforce` as system. Checked by
  writing the file by hand: `mForceIdle=true`, `mState=IDLE` (light state
  OVERRIDE), then back to `ACTIVE`.
- **Idle cost** (60 s windows, container cgroup `cpu.stat` and per-task
  context switches, no Android app in front, Android awake, with the shell no
  longer freezing the container): **0.07 % of one core, 64 context
  switches/s**, 91 processes. The same with deviceidle forced to deep idle:
  0.07 %, 64/s. A 60 s window shows no difference, because deep idle holds
  back alarms, jobs and network over longer periods. For comparison, M0
  measured 0.11–0.30 % and 68–81 context switches/s.

## Sharing

Android apps take part in Marathon's share sheet both ways
(protocol/bridge.md, Share). As targets, the shell asks deckardd's
`Deckard1.Share.ShareTargets` for the activities that take the type and
hands the chosen one the request with `Share`; deckardd copies the files
into `xfer/` and DeckardSystem starts the activity with `Files` URIs. As
sources, DeckardSystem's `Chooser` sits above Android's chooser and sends
what the app shares to deckardd, which opens the sheet with the files as
descriptors and the app as `source`. Host tests cover deckardd's half
(`tests/test_share.cpp`, with a fake sheet on a private bus); not yet run on
the device, and the sheet needs the shell's half.

## Shared storage

Android's shared storage folders are the user's own folders, so a photo an
Android app saves shows in Marathon's Gallery and Files, and a document
saved in Marathon opens in an Android app.

| Android (`/sdcard/…`) | Marathon |
|---|---|
| Documents | `~/Documents` (XDG_DOCUMENTS_DIR) |
| Download | `~/Downloads` (XDG_DOWNLOAD_DIR) |
| Pictures | `~/Pictures` (XDG_PICTURES_DIR; Marathon's camera saves in `Pictures/Marathon`) |
| Music | `~/Music` (XDG_MUSIC_DIR) |
| Movies | `~/Videos` (XDG_VIDEOS_DIR) |
| DCIM | `~/DCIM` (Android cameras save in `DCIM/Camera`) |

`Android/data` and `Android/obb`, and anything else in `/sdcard`, stay in
Android's own data.

**Waydroid has nothing for this** (1.6.3 and main at db7294c4): its session
config binds only the Wayland and PulseAudio sockets and the user's data;
its documentation suggests bind-mounting folders by hand.

**Android 13's stack in the container.** Apps see `/storage/emulated/0`
through MediaProvider's FUSE daemon, which works on `/data/media/0` (on the
host `~/.local/share/waydroid/data/media/0`) as MediaProvider's own app
uid, in group media_rw (1023, from WRITE_MEDIA_STORAGE). MediaProvider
decides per app who may see what (storage permissions, ownership in its
database); the lower filesystem only has to let MediaProvider in. Android
apps never reach `/data/media` directly (0770 media_rw). The host user is
uid 1000, which is Android's system uid; SELinux is off, so file modes and
ACLs are all there is.

**Mounting.** The LXC mount hook `deckard-shared-folders` (pipeline patch
0046 adds `lxc.hook.mount` to Waydroid's `config_base`) runs as root in the
container's mount namespace before `pivot_root`. It finds the session's
data from Waydroid's `config_session` (the `data` bind), the user from its
owner and the folder names from the user's `user-dirs.dirs`, one level under
the home only. It creates missing folders (the user's as the user's,
Android's as 1023:1023 0770, `/data/media/0` too on a first start) and
binds each user folder over Android's, without `MS_REC`. Both trees are the
user's to change, so every path is opened one component at a time with
`O_NOFOLLOW` and mounted through `/proc/self/fd`: a link in either can't
point a root mount anywhere else, and a folder not owned by the user is not
shared. The mounts live in the container's namespace and go with it.
Whatever an Android folder held before is moved into the user's folder
(kept in Android if the name is taken), owned by the user, group media_rw,
group read and write.

**vold's pass-through view.** MediaProvider's FUSE daemon doesn't work on
`/data/media` itself but on `/mnt/pass_through/0/emulated`, which vold
makes at boot as a bind of `/data/media` without its submounts. So the
binds over `/data/media/0/<folder>` alone don't reach `/sdcard` (seen on the
device: `/data/media/0/Pictures` showed the user's folder, `/sdcard/Pictures`
still Android's). `deckard-storage` (an overlay rc service, started at
`post-fs-data`) waits for vold's mount and binds each shared folder there
too; the mount point is shared, so the binds reach every app's namespace,
MediaProvider's included.

**Permissions both ways, nothing world-writable.** deckardd gives each
folder an access ACL with group media_rw rwx and a default ACL with
media_rw rwx and the user rwx, and once (the first time it runs) the same to
everything inside that the user owns (`SharedStorage`, written as the
kernel's `system.posix_acl_*` xattrs). Files the user's apps create inherit
read and write for MediaProvider; files MediaProvider creates (owned by its
uid) inherit read and write for the user. New entries made on the host get
the ACL as deckardd sees them. A file whose mode a program narrows with
`chmod` loses the group's write (the mask), and stays readable.

**Indexing.** Files added or removed on the host bypass the FUSE daemon, so
MediaProvider doesn't see them. deckardd watches each folder and its
subfolders and, 3 s after a change, sends DeckardSystem `media_scan` with
the folders' Android paths; DeckardSystem calls `MediaStore.scanFile` on
each, which indexes new files and forgets gone ones. When Android comes up,
all six folders are scanned once, for what changed while it was off.
Changes deeper than one level under a folder are indexed on the next scan of
it or MediaProvider's idle scan.

**Security.** Android apps reach only these six folders of the user's home,
through MediaProvider's rules; nothing else of the home is mounted, links
inside the folders resolve inside the container, and submounts are not
carried over.

Host tests: `host/deckard-host-tools/tests/shared_folders.rs` (mapping, creation, moving
Android's files, refusing links and data outside the home) and
`test_sharedstorage` (ACLs, inheritance, the paths sent for scanning).

On the HackberryPi (deckard a5362f4 with the hook, `deckard-storage` and the
ACLs staged by hand, scans triggered with `content call … scan_file` in
place of the newer deckardd): MediaProvider is u0_a29 (10029) with media_rw
among its groups, and Android's own `/data/media/0` folders already carry
the same media_rw ACLs. All six folders were bound in `/data/media/0` and
in vold's view; Markor's folder and the `.thumbnails` folders moved to the
user's. A `screencap` into `/sdcard/Pictures` appeared as
`~/Pictures/android-shot.png`, owned by 10029, with ACL `user:1000:rwx` and
`group:media_rw:rwx` (mask rw), readable and writable by the user. A picture
copied into `~/Pictures/Marathon` on the host showed in `/sdcard` and in
MediaStore after the scan, and was gone from MediaStore after it was
deleted on the host and scanned again. `Android/data` and `Android/obb` stay
under `/data/media/0/Android`; nothing in the shared folders was
world-writable. Not yet seen in Marathon's Gallery on screen: the
screenshot showed an Android window over it.

## A boot that never completes

The first start after a reset hung for about six minutes: the composer HAL
had connected to Marathon's Wayland server but never registered, so
SurfaceFlinger waited for it and system_server looped in its watchdog. One
session stop and start fixed it, as it did the transient hang at 01:42.

deckardd's `BootWatchdog` follows the session unit. Android's init writes
`/data/deckard/booted` at `sys.boot_completed` (`deckard-watch.rc`, removed
again at `post-fs-data`). If that file is not newer than the unit's
activation 60 s later, deckardd runs `pkexec /usr/libexec/deckard-boot-evidence`.
Its output goes to deckardd's journal (`deckard.boot`): the container's
freezer state, the boot properties, and for the composer HAL and
SurfaceFlinger their state, Unix sockets (`ss -xp` in the container's network
namespace, the Wayland link among them) and `debuggerd -b` stacks. deckardd
then restarts the session once. If the restarted session stalls as well, it
is only reported; a completed boot re-arms the restart.

Checked on the device by putting a directory where the marker goes, so init
could neither remove nor write it: 60 s after the start deckardd logged the
stall, pkexec ran the helper with no prompt from the user service, and the
journal got the freezer state (not frozen), the boot properties, both
processes' sockets and full `debuggerd -b` stacks (the composer in
`wl_display_dispatch_queue`). It restarted the session once; 60 s after
that it logged "the restarted session stalled too; leaving it". The
composer HAL aborted (SIGABRT in `hwcomposer.waydroid.so`) once or twice at
the start of each of these boots, and init restarted it; worth following up
as the possible cause of the real hang.

## Install

| Piece | Where |
|---|---|
| DeckardBridge.apk | `image/overlay/system/priv-app/DeckardBridge/`, prebuilt and committed (Alpine has no Android SDK; `image/tools/build-apk.sh`, signed with the Deckard key in `~/.marathon-secrets/deckard`) |
| privapp permissions | `image/overlay/system/etc/permissions/privapp-permissions-deckard-bridge.xml` (`MEDIA_CONTENT_CONTROL`, `START_ACTIVITIES_FROM_BACKGROUND`). The image enforces privapp permissions (`ro.control_privapp_permissions=enforce`). |
| Boot settings | `image/overlay/system/etc/init/deckard-bridge.rc`, on `sys.boot_completed=1`: listener access, notification-policy access, no heads-up, no touch sounds or haptics, no notification assistant |
| deckard-bridge | `deckard-bridge.service` (user), `/usr/libexec/deckard-bridge`, wanted by `graphical-session.target` |
| Overlay sync | `deckard-overlay-sync`, `ExecStartPre` of `waydroid-container.service`: copies `/usr/share/deckard/overlay` into `/var/lib/waydroid/overlay` and removes files a previous version shipped |
| Image | Marathon-Shell `packaging/packages/deckard` (aport), `scripts/qemu/lib/build-deckard-apk.sh`, pipeline patch 0044 (HackberryPi) |

The bridge installed as `flags=[ SYSTEM HAS_CODE PERSISTENT ]` with all its
permissions granted, and was bound as a listener 10 s after boot.

On this device the install is by hand until an image carries 0044: the overlay
files are in `/var/lib/waydroid/overlay`, deckardd runs from
`/home/user/.local/deckard` as `deckardd-test.service`, and the session runs
through `~/.config/systemd/user/marathon-waydroid.service.d/deckard-test.conf`.

## Where the spec's approach is still needed

- **Bionic libdbus (spec §5):** not needed for notifications or media. The
  host-side bridge carries them, as docs/roadmap/DECKARD.md recommends.
- **Headless SystemUI notification plumbing:** not needed. A privileged
  listener sees everything SystemUI would.
- **Per-app identity for portals (§6):** not touched.
- **Back with progress (§8):** needs the forked hwcomposer to speak
  `marathon_surface_v1`; stock Waydroid only gets the key on release.

## Cleanup

The 19 rows that Waydroid's own forwarder had left in the Hub ("Configure
physical keyboard" and others) were closed with `CloseNotification`. Rows
deckard posted were left alone; it had none open.
