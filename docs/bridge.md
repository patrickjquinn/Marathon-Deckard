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
- **deckardd** (`host/deckardd`, C++/Qt 6, Apache-2.0) owns the socket, the
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
time and has no replies, removal from the Hub or progress) is turned off. The
session runs through `deckard-waydroid`, which replaces the forwarder's
`start` before running Waydroid's `main`.

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

## Back

The pill's back gesture can stay a Wayland key: no bridge transport is
needed.

- **Path.** For a window without `marathon_surface_v1` (stock Waydroid's
  hwcomposer doesn't speak it), `WaylandCompositor::sendNativeBack` ends in
  `sendKeyTo(Qt::Key_Back)`. Qt Wayland turns `Qt::Key_Back` into the XKB
  keycode of `XF86Back` (`keyToScanCode`) minus 8, which is evdev
  `KEY_BACK` (158). Waydroid's hwcomposer (`wayland-hwc.cpp`,
  `keyboard_handle_key`) writes every key except `KEY_POWER` unchanged into
  `/dev/input/wl_keyboard_events`. Android reads that pipe as a keyboard with
  `Generic.kl`, where `key 158 BACK` is `AKEYCODE_BACK`.
- **Measured.** Writing `EV_KEY 158` down and up into that pipe, exactly as
  the hwcomposer does, finished deskclock's SettingsActivity
  (topResumedActivity went from `SettingsActivity t29` to the home task).
  The same write with no focused window in Android did nothing:
  `dumpsys input` showed `FocusedWindows:` empty.
- **The condition.** Android delivers the key to its focused window, and in
  multi-window mode Android's focus follows `wl_keyboard.enter`: the
  hwcomposer calls `IWaydroidTask::setFocusedTask(taskId)` for the entered
  surface (the call that deadlocks when it races a configure). The shell
  already gives an Android window keyboard focus once it is on screen and its
  configure has settled (`WaylandShellSurfaceItem.takeFocusForKeyboard`), so
  by the time of a back the right task is focused.
- **On the device (shell 4692c6f8, 22:55).** Inconclusive. Notify Test's card
  opened as a black surface, the pill back took the shell Home through the
  unhandled-back watch (no visible change within 350 ms), and Android's
  focused task was Jelly, which Navigation1.LaunchApp had started though it
  returned false. So the window-showing and focus problems come first;
  back's own path is shown above.
- **If it's ever needed:** a bridge `Back(appId)` would still depend on
  Android's focus. A non-platform app can't inject keys (`INJECT_EVENTS` is
  signature-only), and an AccessibilityService's GLOBAL_ACTION_BACK also goes
  to the focused window. It would need `moveTaskToFront` first, which makes
  the hwcomposer reshuffle windows. The Wayland key is the better route.

## Install

| Piece | Where |
|---|---|
| DeckardBridge.apk | `image/overlay/system/priv-app/DeckardBridge/`, prebuilt and committed (Alpine has no Android SDK; `image/tools/build-apk.sh`, signed with the Deckard key in `~/.marathon-secrets/deckard`) |
| privapp permissions | `image/overlay/system/etc/permissions/privapp-permissions-deckard-bridge.xml` (`MEDIA_CONTENT_CONTROL`, `START_ACTIVITIES_FROM_BACKGROUND`). The image enforces privapp permissions (`ro.control_privapp_permissions=enforce`). |
| Boot settings | `image/overlay/system/etc/init/deckard-bridge.rc`, on `sys.boot_completed=1`: listener access, notification-policy access, no heads-up, no touch sounds or haptics, no notification assistant |
| deckardd | `deckardd.service` (user), started by the deckard drop-in on `marathon-waydroid.service` (`Wants=`, and `PartOf=` back) |
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
