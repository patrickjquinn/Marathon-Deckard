# Deckard bridge protocol

The system bridge between Android (inside the Waydroid container) and
Marathon. On the Android side a privileged app, DeckardBridge
(`image/apps/DeckardBridge`, package `org.marathonos.deckard.bridge`), listens
to notifications and media sessions. On the host, `deckard-bridge`
(`host/deckard-bridge`) posts them to Marathon's Hub (`org.freedesktop.Notifications`)
and as MPRIS2 players, and sends the user's taps, actions, replies,
dismissals and media commands back.

Version 1.

## Transport

- A Unix stream socket at `/data/deckard/bridge.sock` inside Android. Waydroid
  bind-mounts the user's `~/.local/share/waydroid/data` as Android's `/data`,
  so deckardd creates it at `~/.local/share/waydroid/data/deckard/bridge.sock`.
  No TCP and no `waydroid0` address is involved; the container's network
  namespace doesn't matter for a socket on the filesystem.
- deckardd listens and DeckardBridge connects, reconnecting with backoff
  (0.5 s doubling to 10 s) whenever the socket is missing or drops.
- The directory is `0755` and the socket `0777`: the bridge runs under its own
  Android app uid. deckardd authenticates every connection with `SO_PEERCRED`.
  The peer's uid must be the uid `/data/system/packages.list` gives
  `org.marathonos.deckard.bridge`; anything else is closed at once. One
  connection at a time: a new authenticated connection replaces the old one.
- SELinux is disabled in the Waydroid container on the devices tested, so no
  policy is involved.

## Framing

Each message is a 4-byte unsigned big-endian length `N` followed by `N` bytes
of UTF-8 JSON. The JSON is an object with a string field `t`, the message type.

- `N` must be 1 to 1,048,576. A larger or zero length, invalid JSON, a
  non-object or a missing `t` closes the connection.
- deckardd accepts a burst of 200 messages and then 50 per second (token
  bucket). Excess messages are dropped and counted; after 1,000 drops the
  connection is closed.
- Unknown types are ignored, in both directions.

## Trust

Everything from the container is untrusted. deckard-bridge bounds and cleans every
field before it reaches a Marathon service (`host/deckard-core/src/validate.rs`):

| Field | Rule |
|---|---|
| package names | `^[A-Za-z][A-Za-z0-9_]*(\.[A-Za-z][A-Za-z0-9_]*)*$`, at most 255 characters |
| `key` | 1 to 512 characters, no control characters |
| text | truncated (label 128, title 256, body 4,096, action label 64, message 1,024 characters); control and format characters except `\n` and `\t` removed |
| action ids | `default` or `a0` to `a99`; at most 5 action buttons |
| images | base64 PNG only: icon 256 KiB, notification image 512 KiB, album art 768 KiB; at most 4096 x 4096; decoded, scaled and re-encoded by deckardd, never passed through |
| numbers | clamped (urgency 0 to 2, progress 0 to 100, media times 0 to 10^11 ms, speed -16 to 16) |
| counts | at most 200 notifications and 8 media players tracked |

The body is HTML-escaped because the Hub advertises `body-markup`.

## Android to host

| `t` | Fields | Meaning |
|---|---|---|
| `hello` | `v` | Sent first after connecting. |
| `icon` | `pkg`, `png` | The app's icon (96 px PNG, base64). Sent once per package per connection, before its first notification. deckardd stores it as `~/.cache/deckard/icons/<pkg>.png`. |
| `post` | see below | A notification was posted or updated. |
| `remove` | `key` | Android removed a notification (the app cancelled it, or it was dismissed). |
| `sync` | `keys` | After the initial `post`s on every connection: the keys of all active notifications. deckardd closes any notification it tracks that isn't listed. |
| `media` | see below | A media session appeared or changed. |
| `media_gone` | `sid` | A media session ended. |
| `inspect_result` | `id`, `ok`, `message`, `pkg`, `label`, `versionName`, `versionCode`, `minSdk`, `targetSdk`, `permissions`, `icon`, `installedVersionCode`, `installedVersionName`, and what it needs: `features` (required `uses-feature`), `libraries` (required `uses-library`, from its manifest), `gms` (Play services' `com.google.android.gms.version` meta-data or library), `abis` (the `lib/<abi>/` folders) | An `inspect` answered from `PackageManager.getPackageArchiveInfo`. |
| `install_progress` | `id`, `percent` | Copying the APK into the PackageInstaller session. |
| `install_result` | `id`, `ok`, `pkg`, `message` | An `install` finished (`PackageInstaller.EXTRA_STATUS`). Removals go to DeckardSystem (`uninstall` on `system.sock`). |
| `packages_result` | `id`, `packages: [{pkg, label, versionName, versionCode, system, launchable}]` | Every installed package; `launchable` is whether it has a launcher activity. |
| `device_result` | `id`, `features`, `libraries`, `abis`, `sdk`, `gms` | This phone's Android, for `device` (host to Android, `id`): `getSystemAvailableFeatures`, `getSystemSharedLibraryNames`, `Build.SUPPORTED_ABIS`, the SDK level and whether `com.google.android.gms` (microG) is installed and enabled. `Packages.DeviceProfile() → a{sv}` answers with it; the Store compares each app's needs with it (compatibility). |
| `package_changed` | `pkg`, `change` | `installed`, `updated` or `removed`, from ACTION_PACKAGE_ADDED/REMOVED (a replace is one `updated`). |
| `location_request` | `on`, `interval` (ms) | Android's network location provider (DeckardBridge's `NetworkLocation`) has a request (`on`) or none. Sent on every change and on connect. deckardd then runs one GeoClue client (system bus, `DesktopId` `org.marathonos.Deckard`, accuracy level exact, `TimeThreshold` the interval in seconds) or stops it. `interval` is clamped to 0–24 h. |
| `toast` | `pkg`, `label`, `text` | An app showed a toast (from the bridge's accessibility service: `TYPE_NOTIFICATION_STATE_CHANGED` with class `android.widget.Toast`). deckardd posts a silent banner (`urgency` 0, `transient`, `suppress-sound`, `x-marathon-toast`, `expire_timeout` 3500, summary = the text, `app_name` = the label) and closes it after 4 s. At most one per package per 500 ms; text up to 512 characters. |

`post`:

| Field | Type | |
|---|---|---|
| `key` | string | `StatusBarNotification.getKey()` |
| `pkg`, `label` | string | Package and app label |
| `title`, `text` | string | `EXTRA_TITLE_BIG` or `EXTRA_TITLE`; `EXTRA_BIG_TEXT` or `EXTRA_TEXT`, or the inbox lines |
| `when` | number | `Notification.when`, ms |
| `ongoing` | bool | `FLAG_ONGOING_EVENT` or `FLAG_NO_CLEAR` |
| `autoCancel` | bool | `FLAG_AUTO_CANCEL` |
| `silent` | bool | Channel importance below default, or an update with `FLAG_ONLY_ALERT_ONCE` |
| `category` | string | `Notification.category`, optional |
| `urgency` | 0–2 | 2 for alarms, calls and full-screen intents; 0 for importance low or min |
| `content` | bool | Has a `contentIntent` (a tap opens something) |
| `actions` | array of `{id, label}` | Action buttons, `id` = `a<index into Notification.actions>`; contextual (smart reply) actions are left out |
| `reply` | `{action, label, placeholder}` | The first action with a free-form `RemoteInput`, optional |
| `progress` | `{max, current, indeterminate}` | Optional |
| `conversation` | `{title, self, group, messages: [{sender, text, time}]}` | `MessagingStyle`, last 10 messages, optional |
| `image` | string | Large icon, 128 px PNG base64, optional |

Not forwarded: MediaStyle notifications (they become MPRIS players), group
summaries while their group has children, the bridge's own notifications, and
Android system noise (`android` package channels `PHYSICAL_KEYBOARD`,
`DEVELOPER`, `DEVELOPER_IMPORTANT`, `USB`, `FOREGROUND_SERVICE`).

`media`:

| Field | Type | |
|---|---|---|
| `sid` | int > 0 | Session id, stable for the connection |
| `pkg`, `label` | string | |
| `state` | `Playing`, `Paused`, `Stopped` | |
| `position` | ms | Position when sent |
| `speed` | number | Playback speed |
| `duration` | ms | 0 when unknown |
| `title`, `artist`, `album` | string | |
| `art` | string | Album art PNG base64, at most 512 px; present only when it changed (`""`: no art) |
| `can` | `{play, pause, next, previous, seek, stop}` | From `PlaybackState.getActions()` |

## Host to Android

| `t` | Fields | Meaning |
|---|---|---|
| `hello` | `v` | Sent on connect. |
| `invoke` | `key`, `action` | Send the notification's `contentIntent` (`default`) or an action's `PendingIntent` (`a<n>`). A tap on an auto-cancel notification also cancels it. |
| `reply` | `key`, `action`, `text` | Fill the action's `RemoteInput` with `text` (`SOURCE_FREE_FORM_INPUT`) and send its `PendingIntent`. |
| `dismiss` | `key` | `cancelNotification(key)`. Android ignores it for ongoing notifications. |
| `media_cmd` | `sid`, `cmd`, `position` | `play`, `pause`, `playpause`, `stop`, `next`, `previous`, or `seek` to `position` ms, through the session's `TransportControls`. |
| `inspect` | `id`, `path` | Read an APK deckardd staged at `/data/deckard/install/<random>.apk`. |
| `install` | `id`, `path` | Install it through a PackageInstaller session (the bridge holds INSTALL_PACKAGES, so Android asks nothing). |
| `packages` | `id` | List installed packages. |
| `location` | `lat`, `lon`, `acc` (m), `time` (ms since the epoch), optional `alt` (m), `speed` (m/s), `heading` (degrees) | A GeoClue fix while a request is on (`src/location.cpp`): latitude −90–90, longitude −180–180, accuracy above 0, GeoClue's "unknown" altitude, speed and heading left out. The provider reports it as a `network` location; Android decides which apps get it and coarsens it for apps with approximate access. GeoClue asks the shell's agent, which lets `org.marathonos.Deckard` locate, since Android asks for each app. |
| `audio` | `quiet`, `alarm` | Marathon's sound state, sent on connect and on every change. `quiet` is true when Marathon's profile is silent or vibrate or Do Not Disturb is on; the bridge then requests `INTERRUPTION_FILTER_ALARMS` (Marathon's own alarms ring through all three), otherwise `INTERRUPTION_FILTER_ALL`. Either way Android's notification, ring and system streams stay at 0 (the Hub plays Marathon's notification sound, which follows the profile), the music stream is at its maximum under Marathon's output volume, and the alarm stream is `alarm` (0 to 1) of its maximum. deckardd reads these from `org.marathonos.Shell.Settings1.GetState` (`audioProfile`, `dndEnabled`, `alarmVolume`) and re-reads on `Audio1.StateChanged`, `Settings1.PropertyChanged` and a shell restart. |

## Mapping to Marathon

Notifications go to `org.freedesktop.Notifications` on the session bus:

| freedesktop | From |
|---|---|
| `app_name` | `label` |
| `replaces_id` | The id deckardd got for this `key` |
| `app_icon` | `file://~/.cache/deckard/icons/<pkg>.png` |
| `summary`, `body` | `title`, `text` (escaped) |
| `actions` | `default` ("Open") if `content`; `inline-reply` if `reply`; then the action buttons |
| `expire_timeout` | 0 if ongoing, else -1 |
| hint `desktop-entry` | `waydroid.<pkg>`, the id Waydroid gives the app's window and desktop file, so the Hub groups by app and a tap opens it |
| hint `image-data` | `image`, re-encoded RGBA |
| hint `urgency`, `category`, `resident`, `suppress-sound` | `urgency`; `category` mapped (`msg` → `im.received`, `email` → `email.arrived`, `call` → `call.incoming`, `missed_call` → `call.unanswered`, `progress` → `transfer`, `social` → `presence`, `err` → `device.error`, `sys` → `device`, others `x-android.<category>`); `ongoing`; `silent` |
| hint `value` | Progress in percent, -1 when indeterminate (the Hub's ongoing-progress row) |
| hint `x-kde-reply-placeholder-text` | `reply.placeholder` |
| hint `x-marathon-conversation` | `conversation` as compact JSON. The Hub carries it but doesn't render threads yet. |

A row the user closes (`NotificationClosed` reason 2) stays matchable for
500 ms, because the Hub can report the close of a tapped row before the tap
itself.

An unchanged `post` (same digest of everything sent to the Hub) is not
re-sent, so updates that change nothing visible make no new banner or sound.

From the Hub: `ActionInvoked` becomes `invoke`; `NotificationReplied` becomes
`reply`; `NotificationClosed` with reason 2 (dismissed by the user) becomes
`dismiss`, except within 3 s of a tap on a notification that doesn't
auto-cancel (the Hub closes a tapped row; Android keeps it). deckardd keeps
`key` → id in `~/.local/state/deckard/notifications.json`, so a restart
neither duplicates nor orphans Hub rows.

Media sessions become `org.mpris.MediaPlayer2.waydroid_<pkg with _>.instance<sid>`,
each on its own bus connection, with `Identity` = label, `DesktopEntry` =
`waydroid.<pkg>`, `Metadata` (`mpris:trackid`, `xesam:title`, `xesam:artist`,
`xesam:album`, `mpris:length`, `mpris:artUrl` = a PNG under
`~/.cache/deckard/art/`), `PlaybackStatus`, `Position` (extrapolated from the
last report), the `Can*` properties, `Seeked`, and Play, Pause, PlayPause,
Stop, Next, Previous, Seek and SetPosition. Players go away when the bridge
disconnects and come back when it reconnects.

## Keel Actions

Android apps as Keel Actions providers, so that keel-mcp and Pilot can use
them as they use Marathon apps. Most messages share the bridge socket.
Launcher shortcuts and secure windows go over the system channel (below),
because only DeckardSystem's uid may read them.

deckard-bridge (`host/deckard-bridge/src/keel.rs`) owns the session bus name
`waydroid.<pkg>` for each launchable app, on a bus connection of its own,
and serves `org.shipwright.Keel.Actions` at `/org/shipwright/Keel/Actions`.
This is the app id Waydroid gives the app's window and desktop file, and the
one the shell's `apps.list` reports. keel-mcp finds these names by calling
`Describe` on each name it sees (there is no installed manifest for them).
keel-mcp describes a name again only when its owner changes. So when an
app's manifest changes, deckardd releases the name and takes it again on a
new connection. The catalogue is kept in `~/.local/state/deckard/apps.json`,
so the names are back as soon as deckardd starts, before Android is up.

`Invoke` and `GetContext` are served only to `/usr/bin/keel-mcp` and
`/usr/bin/pilotd`, compared by file (device and inode of
`/proc/<pid>/exe`). Other callers get `AccessDenied`; `Describe` is open
to everyone. A keel-mcp started with `--external` (a paired computer) gets no
screen content. Every action except `screen.read` and the context is refused
while the phone is locked (`org.marathonos.shell` `shell.state`).

### Android to host

| `t` | Fields | Meaning |
|---|---|---|
| `app_info` | `pkg`, `label`, `links`, `share`, `search` | One launchable app. Sent for every app after each connect, and again when the package changes. `links`: `[{s, h}]`, the scheme and host of each `VIEW` + `BROWSABLE` intent filter of an exported activity, read from the app's compiled `AndroidManifest.xml` (at most 16). `share`: an activity takes `ACTION_SEND` `text/plain`. `search`: `gms` for `com.google.android.gms.actions.SEARCH_ACTION` (the assistant's search intent), `android` for `ACTION_SEARCH`, or `""`. |
| `app_gone` | `pkg` | The app was removed or has no launcher activity left. |
| `app_list` | `pkgs` | After the `app_info`s of a connect: every app. deckardd withdraws any app not listed. |
| `launched` | `req`, `ok`, `error` | The answer to `launch`. |
| `screen` | `req`, `ok`, `error`, `pkg`, `snap`, `title`, `nodes` | The answer to `screen_read`. `snap` is a snapshot number (1 to 2^31-1). `nodes` are in tree order, at most 300: `{i, d, c, t, cd, h, v, f}` = index in this snapshot, depth among the nodes sent, class name, text, content description, hint, view id, flags. Flags: 1 clickable, 2 long-clickable, 4 editable, 8 checkable, 16 checked, 32 selected, 64 disabled, 128 focused, 256 scrollable, 512 password, 1024 heading. A node is sent when it is visible and has text, a description, a hint, or one of flags 1, 2, 4, 8, 256 or 512. A password field never has `t`. |
| `ui_done` | `req`, `ok`, `error` | The answer to `ui_act`. |

### Host to Android

| `t` | Fields | Meaning |
|---|---|---|
| `launch` | `req`, `pkg`, `kind`, then per kind | Start an activity of `pkg`, with `FLAG_ACTIVITY_NEW_TASK`. `view`: `uri`, as `ACTION_VIEW` + `BROWSABLE`. `send`: `text`, `subject`, as `ACTION_SEND` `text/plain`. `search`: `query`, `mode` (`gms` or `android`). Each is limited to `pkg` (`setPackage`) and must resolve with `MATCH_DEFAULT_ONLY`. |
| `screen_read` | `req`, `pkg` | Describe the window of `pkg`. |
| `ui_act` | `req`, `pkg`, `snap`, `node`, `act`, `text` | `act` `click` (`ACTION_CLICK` on the node or its nearest clickable ancestor) or `set_text` (`ACTION_SET_TEXT`, not on a password field) on node `node` of snapshot `snap`. Refused unless `snap` is the bridge's latest snapshot and the node is still on screen in `pkg`. |

The screen comes from DeckardBridge's `ScreenReader` accessibility service
(`canRetrieveWindowContent`, `flagRetrieveInteractiveWindows`,
`flagReportViewIds`, no event types). It is bound only on demand. A
`screen_read` or `ui_act` adds it to `enabled_accessibility_services` (with
`WRITE_SECURE_SETTINGS`, a privapp permission), and it is removed again two
minutes after the last request. The window it reads is the `TYPE_APPLICATION`
window whose root belongs to `pkg`, the focused one if there are several.

deckardd waits up to 15 s for a reply, then fails the call. A request made
while the bridge is down is sent when the bridge connects, within the same
15 s; requests already sent fail when the bridge drops. Bounds: label 128
characters; shortcut ids 1 to 128 without control characters (taken as
sent, never rewritten), labels 64, long labels 256; schemes
`[a-z][a-z0-9+.-]{0,31}` other than `file`, `content`, `javascript`,
`data`, `intent`, `android-app`, `about`, `jar`, `blob` and
`android.resource`; hosts `[a-z0-9*][a-z0-9.*-]{0,252}`; node text 1,000,
description 500, hint 200, class and view id 128, at most 400 nodes; error
strings 256. Deckard's own packages are never published.

### On the system channel

DeckardSystem (`AppActions.java`) answers these with the system uid, which
is a shortcut host (it holds `ACCESS_SHORTCUTS`, `signature|role|recents`)
and may take the window manager's dump.

| Direction | `t` | Fields | Meaning |
|---|---|---|---|
| host to Android | `keel` | `req`, `op`, `pkg`, `id` | `op` `shortcuts`: the app's launcher shortcuts. `start`: `LauncherApps.startShortcut(pkg, id)`. `secure`: whether a window of `pkg` that has a surface is `FLAG_SECURE` (`dumpsys window windows`: the `fl=` list of each window block whose `package=` is `pkg` and that has `mHasSurface=true`). |
| Android to host | `keel_done` | `req`, `ok`, `error`, `shortcuts`, `secure` | The answer. `shortcuts`: `[{id, label, long}]`, the manifest shortcuts and then the dynamic ones, each in rank order, at most 15. A `secure` query that finds no window of the app fails. |
| Android to host | `keel_changed` | `pkg` | `LauncherApps.Callback.onShortcutsChanged`: deckardd asks for that app's shortcuts again. |

deckardd asks for every app's shortcuts when the system channel connects
and after each `app_info`. The last list it got is kept with the app, in
`apps.json` too.

### The manifest

`Describe` returns a Keel manifest (`version` 1, `appId` `waydroid.<pkg>`,
`context` `keel://waydroid.<pkg>/context`) with these tools:

| Action | Arguments | Hints | What deckardd does |
|---|---|---|---|
| `app.show` | `link` (only if the app declares links; up to 2,048 characters) | idempotent, not destructive | Without a link: `org.marathonos.shell` `app.launch {id}`. With a link: `launch` `view`, then the shell's `app.launch` once the app's window is filed (below). The description lists up to 8 of the app's link schemes and hosts. |
| `share.text` | `text` (20,000), `subject` (200) | open world | `launch` `send`. The app's share screen opens; the person sends from there. |
| `app.search` | `query` (500) | idempotent | `launch` `search` |
| `shortcut.<word>` | none | `confirm` and destructive when the label says delete, remove, erase, wipe, clear, reset, send, pay, buy, order, purchase, transfer, call, dial, post, publish, record, uninstall, block, report, sign out or log out | `keel` `start` on the system channel. `<word>` is the shortcut id in lower case with runs of other characters as `_`, prefixed `s` if it does not start with a letter, at most 40 characters, with `_2`, `_3`... when two collide. The title is `<label> (<app>)`. |
| `screen.read` | none | read-only, `untrusted` | Only for the app in front of an unlocked, lit screen (`shell.state` `foregroundApp`), and only when DeckardSystem says no window of the app is secure (`keel` `secure`; without DeckardSystem, never). `screen_read`, then one line per node: `[<snap>.<i>] <role> "<text>" (<description>) hint "<hint>" #<view id> {<state>}`, indented by depth, at most 16,000 characters (`truncated` says if more were cut). Roles: `field`, `checkbox`, `switch`, `radio`, `button`, `image`, `web`, `slider`, `progress`, `list`, `text`, `item` (clickable), `group`. Result `{app, label, title, snapshot, screen, truncated}`. |
| `ui.tap` | `node` (`<snap>.<i>`) | `confirm`, destructive | Only with `userInitiated` from Pilot (keel-mcp sets it only for pilotd) and for the app in front; `node` must be from the latest `screen.read`. `ui_act` `click`. |
| `ui.type` | `node`, `text` (4,000) | `confirm`, destructive | As `ui.tap`, `ui_act` `set_text`. |

keel-mcp refuses `confirm` and destructive tools to every client but Pilot
(`NeedsConfirmation`), and Pilot asks the person before each of them.
Pilot also asks before open-world tools (`share.text`).

`GetContext` returns `{app, purpose, text}`, with `text` the `screen.read`
lines (at most 4,000 characters), when the app is in front and not secure;
otherwise `{}`.
keel-mcp reads it only for a request Pilot says the person started.

**Secure windows and the shell.** On the shell's
`Navigation1.ForegroundAppChanged(appId)` for a `waydroid.*` app, and after
every `secure` answer, deckardd tells the shell
`Navigation1.SetAppWindowSecure(appId, secure)` when that changes. The shell
then keeps the app's windows off its card and out of screenshots. A window
still starting is not in the dump yet, so a failed check is retried once
after 2 s.

**Bringing the window forward.** An activity that Android starts for an
action maps a window nobody launched from Marathon, and the shell files
such a window as a card. Before the `launch` (or `keel` `start`), deckardd
calls `org.marathonos.Shell` `Navigation1.ExpectAppWindow(appId) -> b`
(shell 24c813f2). When it answers true, the shell shows the app's window
now, and a new window of the app (an activity in a new task) takes the front
as a launch would. When the call fails (a shell without it), deckardd falls
back. It polls the shell's `apps.running` every 250 ms, for up to 15 s,
until `waydroid.<pkg>` is listed, then calls `app.launch`, which shows the
window the shell already has. Launching through the shell first would race
Waydroid's launcher intent against the action's. When Android is not
running, deckardd calls `app.launch` first, which starts the session, and
sends the request when the channel connects.

## Ambience palette

Not a socket message. deckardd writes `/data/deckard/palette`
(`~/.local/share/waydroid/data/deckard/palette`), one `system_<palette>_<shade>
#aarrggbb` line for each of Android's 65 system colours. It writes the file
whenever Marathon's ambience highlight changes (`Ambience1.AmbienceChanged`,
`GetAmbience`), with the same generator as DeckardPalette's gen-palette.py
(`palette.cpp`, checked against it in `tests/test_palette.cpp`). Android's
`deckard-watch` service (`inotifyd` on `/data/deckard`, user system) runs
`deckard-palette.jar` under `app_process`. That registers one fabricated
overlay `android:deckard_ambience` on `android` with those colours and enables
it, in a single overlay-manager transaction (enabling also makes it the
highest priority, above DeckardPalette). Lines that don't match
`system_(accent[123]|neutral[12])_N #AARRGGBB` are ignored.

## IME channel

Marathon's keyboard in Android apps. DeckardIme (`image/apps/DeckardIme`,
package `org.marathonos.deckard.ime`, `/system/app` in the overlay) is
Android's only input method; it draws nothing. deckard-bridge's IME channel
(`host/deckard-bridge/src/ime.rs`) relays between it and the shell's
`org.marathonos.Shell.TextInput1`.

Transport: the same framing, limits and `SO_PEERCRED` check as the bridge, on
its own socket `/data/deckard/ime.sock` (`~/.local/share/waydroid/data/deckard/ime.sock`),
accepting only the uid `packages.list` gives `org.marathonos.deckard.ime`. A
separate socket so keystrokes never wait behind 1 MiB image frames.

Text offsets are UTF-16 code units on both sides (Java and Qt strings), so
nothing is converted.

### Android to host

| `t` | Fields | Meaning |
|---|---|---|
| `start` | `package`, `purpose`, `hints`, `inputType`, `imeOptions` | An editor gained input (`onStartInput`). `purpose` and `hints` are `zwp_text_input_v3` content purpose and hint bits mapped from `EditorInfo.inputType` (`ContentType.java`); the raw values are informational. Also sent for Android's fallback connection on non-editor windows (`inputType` 0, purpose `terminal`), which never asks for the keyboard. |
| `state` | `text`, `cursor`, `anchor` | Up to 256 characters either side of the cursor, and the selection within them. Empty for password and PIN fields. Sent on start and on every selection change. |
| `show` | | Android asked for the keyboard (`onShowInputRequested`). |
| `hide` | | Android hid the keyboard (`hideWindow`). |
| `finish` | | The editor lost input (`onFinishInput`). |

deckardd checks `package` against the package-name rule, clamps `purpose` to 0–13,
masks `hints` to `0x3ff`, drops `state` over 4,096 characters and clamps the
offsets. It ignores everything but `start` until a valid `start`.

### Host to Android

| `t` | Fields | Meaning |
|---|---|---|
| `commit` | `text`, `before`, `after` | Replace `before` characters before and `after` after the cursor with `text` (both 0: insert, or replace the selection). DeckardIme applies a replacement as one edit by selecting the range and committing over it. |
| `key` | `key` | `enter`, `backspace`, `left`, `right`, `up`, `down`, `tab` or `escape` from the on-screen keyboard. `enter` runs the editor's IME action (Go, Search, Send, Next, Done) when it has one and is single-line, inserts a newline in multi-line fields, and is a plain Enter key otherwise. The rest are key events through the input connection, so they stay in order with commits. |
| `panel` | `covered`, `height` | How much of the app's window Marathon's keyboard covers, out of the window's height, in the shell's logical pixels (0 when it is down). DeckardIme scales it to Android pixels, makes its invisible window report that inset, and shows or hides itself to match. Sent when it changes for the editor's app, and replayed on `start`. |

### Host to shell

`org.marathonos.Shell` `/org/marathonos/Shell/TextInput`
`org.marathonos.Shell.TextInput1`. App ids are `waydroid.<package>`.

| Method | From |
|---|---|
| `Register(s prefix)` | deckard-bridge at start and whenever the shell reappears, with `waydroid.`. Needs the caller to be a declared session service (`/usr/share/marathon-shell/services/deckard-bridge.service.json`, permission `text-input`); the last caller owns the channel. |
| `SetEditor(s appId, u purpose, u hints)` | `start` |
| `SetSurroundingText(s appId, s text, i cursor, i anchor)` | `state` |
| `ShowPanel(s appId)` / `HidePanel(s appId)` | `show` / `hide`; also `HidePanel` when the IME disconnects |
| `ClearEditor(s appId)` | `finish`, or the IME disconnecting |

Targeted signals from the shell to the owner: `CommitString(s appId, s text,
i before, i after)` → `commit`, `Key(s appId, s key)` → `key`,
`PanelChanged(s appId, i covered, i height)` → `panel`.

The shell shows the keyboard for `ShowPanel` only when that app's window has
focus and no hardware keyboard is present, answers the keyboard's queries for
that window from the reported editor, and does not shrink the windows of
apps under the registered prefix when the keyboard is up (resizing a Waydroid
toplevel hot-plugs Android's display). Hardware keys reach Android over
`wl_keyboard` as before.

## System channel

Intents that Marathon apps answer, for an Android trimmed to a runtime
([../docs/slim.md](../docs/slim.md)). DeckardSystem (`image/apps/DeckardSystem`,
package `org.marathonos.deckard.system`, `/system/priv-app`) is signed with
AOSP's platform test key and runs as `android.uid.system`. Its activities
take links, `tel:`/`sms:`/`mailto:`/`geo:` URIs, settings pages, the file
pickers and the camera intents. deckard-bridge's intents
(`host/deckard-bridge/src/intents.rs`) hands each one to a Marathon app.

Transport: the same framing, limits and `SO_PEERCRED` check, on
`/data/deckard/system.sock`, accepting the uid `packages.list` gives
`org.marathonos.deckard.system`. That uid is 1000, Android's `system`, which
is also the host user's uid (the container has no user namespace). So the
check admits Android's own system processes and this user's host processes,
nothing that runs as an Android app uid.

Files cross through `/data/deckard/xfer/<id>/<name>`
(`~/.local/share/waydroid/data/deckard/xfer/…`), owned by uid 1000 on both
sides. Apps read them through DeckardBridge's DocumentsProvider
`Handoff` (`org.marathonos.deckard.bridge.files`, document id `<id>/<name>`).
Android takes no URI grant from the system uid, so DeckardSystem doesn't
grant: it opens the files up for DeckardBridge's uid (they keep the modes of
their originals) and asks `Handoff` (`call("grant")`, from the system uid
only) to grant the app read access, plus write for a save and persistable for
`ACTION_OPEN_DOCUMENT`/`CREATE_DOCUMENT`. A save's file is closed through
`Handoff`, which sends `pick_done` on the bridge channel. deckardd removes
transfers older than 30 days at start.

### Android to host

| `t` | Fields | Meaning |
|---|---|---|
| `hello` | `v` | Sent first after connecting. |
| `open` | `kind`, then per kind | Show something in a Marathon app. `url`: `uri` (http or https). `tel`: `uri`. `sms`: `uri`, `body`. `mailto`: `uri`, `to`, `cc` (arrays), `subject`, `body`. `geo`: `uri`. `settings`: `action` (the Android settings action), `pkg` (the app it is about, optional). |
| `pick` | `id`, `mode`, `mime`, `multiple`, `name`, `kind` | A file or camera request. `id` is 16 lowercase hex digits, chosen by DeckardSystem. `mode` `open` (ACTION_OPEN_DOCUMENT, ACTION_GET_CONTENT; `mime` the accepted types, `multiple`), `save` (ACTION_CREATE_DOCUMENT; `name` the suggested file name) or `capture` (ACTION_IMAGE_CAPTURE, ACTION_VIDEO_CAPTURE; `kind` `image` or `video`). |
| `pick_done` | `id` | The app closed a file it wrote for a `save`; copy it to where the person chose. Sent on every close. |
| `pick_cancel` | `id` | The app or the person gave up; deckardd forgets the request. |
| `reclaimed` | `id`, `ok`, `killed` (`[{pid, process}]`) or `error` | The answer to a `reclaim`. |
| `install_result` | `id`, `ok`, `pkg`, `message` | The answer to an `uninstall`, as DeckardBridge's. |

### Host to Android

| `t` | Fields | Meaning |
|---|---|---|
| `hello` | `v` | |
| `picked` | `id`, `ok`, `files` | The answer to a `pick`. For `open` and `capture`, `files` (`[{name, size}]`) are in `xfer/<id>/`. For `save`, one `{name}`: DeckardSystem creates the empty file there and gives the app write access. `ok` false: cancelled or failed. |
| `focus` | `pkg` | Marathon has `waydroid.<pkg>` in front (deckardd follows `org.marathonos.Shell.Navigation1.ForegroundAppChanged`; sent on every change and on connect). Android's focus normally follows the hwcomposer's `wl_keyboard.enter`; when that never came, DeckardSystem calls `setFocusedTask` on the package's top visible task, 150 ms later, unless Android already has it focused. |
| `uninstall` | `id`, `pkg` | `Packages.UninstallPackage`: remove an app the person installed, through `PackageInstaller.uninstall` as the system uid, so with no confirmation whoever installed it. System apps and Deckard's own are refused. |
| `media_scan` | `paths` (at most 64) | Folders of Marathon's shared storage, as Android sees them (`/storage/emulated/0/<Documents, Download, Pictures, Music, Movies or DCIM>[/…]`), that changed on the host. DeckardSystem calls `MediaStore.scanFile` on each; other paths are ignored. |
| `reclaim` | `id`, `count` (1–8), `reason` | Memory is short: end up to `count` cached app processes, least recently used first. Only processes ActivityManager ranks `IMPORTANCE_CACHED`, with an app uid (10000 and up) and not Deckard's own, are candidates; each goes through `killPids` without `secure`, which re-checks its adj and spares one that came forward. deckardd sends it when the container's `memory.events` `high` or `max` counter rises, on a PSI trigger on the container's `memory.pressure` (`some 1000000 10000000`, 10 % of 10 s) or the phone's `/proc/pressure/memory` (`some 2000000 10000000`, the shell's avg10 High level), with `count` 2, at most once every 2 s, or 10 s after a reclaim that found nothing. |

### Where each goes

| From Android | To Marathon |
|---|---|
| `url` | Keel `org.marathonos.browser` `browser.open {url, newTab}` |
| `tel` | `Navigation1.LaunchAppWithRoute("phone", "/call", {number})`: the dialler with the number filled in, not a call. No number: `LaunchApp("phone")`. |
| `sms` | `LaunchAppWithRoute("messages", "conversation", {number, body})`: the thread, with `body` (or the URI's `?body=`) as its draft. No number but a body: `LaunchAppWithRoute("messages", "compose", {body})`, a new message to pick the recipient for. Neither: `LaunchApp("messages")`. |
| `mailto` | Keel `org.marathonos.email` `mail.compose {to, cc, subject, body}` |
| `geo` | Keel `org.marathonos.maps` `map.show {lat, lon, name}` for coordinates (or a `q=lat,lon(name)`); `search.show {query}` for a text query (Maps' search page with the results); `LaunchApp("maps")` for neither |
| `settings` | `LaunchAppWithRoute("settings", page)`: Wi-Fi, Bluetooth, sound, display, date and time, storage, battery, security, keyboard, accounts, about, notifications and the app manager pages; `notifications/waydroid.<pkg>` and `appmanager/waydroid.<pkg>` for one app. Special access (all files, installing apps, overlay, system settings, usage, notification access, Do not disturb, battery, alarms, picture in picture, accessibility): `androidaccess/<kind>`, or `androidaccess/<kind>/<pkg>` when the request names the app, which shows only its switch. Others (location): `LaunchApp("settings")`. |
| `pick` `open` / `save` | `org.freedesktop.portal.FileChooser` `OpenFile` / `SaveFile`, with a MIME filter unless the app accepts `*/*` |
| `pick` `capture` | Keel `org.marathonos.camera` `camera.capture {mode}` (`photo` or `video`), which answers once the person has shot, with `{path, mode}`, and minimises the camera (the call waits up to 10 minutes). The file must be in `~/Pictures/Marathon` and of the kind asked for, or the pick fails. `Cancelled` (the person left the camera), `NotAvailable` or `Timeout` fail it too. A `pick_cancel` drops the request; the camera stays for the person. |

deckardd's checks (`tests/test_intents.cpp`): `uri` at most 4,096 characters
and of the kind's scheme; a URL needs a host and at most 2,048 characters;
numbers keep only digits, `+*#,;pw` and at most 64 of them (anything else and
the app opens without one); addresses have no spaces, commas, `<>"`, at most
254 characters and 20 of each; a mail's subject and body are cut to 1,000 and
100,000 characters, a text message's body to 10,000 and a map search to 256;
settings actions come from a fixed table; `id` must match
`^[0-9a-f]{16}$`; at most 4 picks wait at once; file names are reduced to a
base name without control characters, `/` or `\`.

## Share

Phases 2 and 3 of Marathon's share sheet. Phase 1 is in the shell: the shell
owns `org.sailfishos.share` on the session bus (Keel's `ShareAction` calls
`share(a{sv})` on `/`), shows the sheet, and hands the request to the
Marathon app the person picks, on its `org.marathonos.<id>` name at
`/share/<method>` (the app's manifest lists its `shareMethods`). Text
resources are `{name, data}` maps; files are `{name, fileDescriptor}` maps.

### Android apps as targets

`org.marathonos.Deckard1.Share` on `/org/marathonos/Deckard`, callable only
by the shell (the sender's pid is the pid owning `org.marathonos.Shell`;
anyone else gets `AccessDenied`):

| Method | |
|---|---|
| `ShareTargets(s mimeType) → aa{sv}` | The activities that take `ACTION_SEND` for that type: `{id, pkg, label, icon, multiple}`. `id` is `<pkg>/<activity class>` (at most 256 characters), `icon` a `data:image/png` URI (96 px) or `""`, `multiple` whether the activity also takes `ACTION_SEND_MULTIPLE`. At most 64; never Deckard's own. Empty while Android is not running; it is not started just to answer. |
| `Share(s id, a{sv} configuration) → ()` | The shell's request, as the sheet received it: `mimeType`, `title`, `resources` (at most 32; text of 1 to 65,536 characters; files as regular-file descriptors, 1 GiB in all). deckardd copies each file to `xfer/<tid>/<name>` (`tid` 16 hex digits, names reduced to a base name and made unique), calls `Navigation1.ExpectAppWindow("waydroid.<pkg>")` so the window Android opens takes the front, and sends `share` on the system channel. Replies once Android started the activity; `org.marathonos.Deckard1.Error.Failed` otherwise (15 s at most), `InvalidArgs` for a bad request. |

System channel:

| `t` | Direction | Fields | Meaning |
|---|---|---|---|
| `share_targets` | host to Android | `id`, `mime` | DeckardSystem queries `ACTION_SEND` and `ACTION_SEND_MULTIPLE` activities (exported, default category). |
| `share_targets_result` | Android to host | `id`, `targets` (`[{pkg, activity, label, icon, multiple}]`, `icon` base64 PNG) | deckardd checks each: package and class names, Android label rules, the icon decoded and re-encoded. |
| `share` | host to Android | `id`, `target`, `mime`, `title`, `text`, `files` (names in `xfer/<id>/`) | DeckardSystem starts `target` with `ACTION_SEND` (`EXTRA_TEXT`, `EXTRA_SUBJECT` from `title`, `EXTRA_STREAM`) or, for several files, `ACTION_SEND_MULTIPLE`, the streams being `Handoff` document URIs (`content://org.marathonos.deckard.bridge.files/document/<id>%2F<name>`), granted to the target's package for reading through `Handoff` before the start, and in the `ClipData`. |
| `shared` | Android to host | `id`, `ok`, `error` | |

The shell lists these targets after the Marathon apps, only while
`org.marathonos.Deckard` is on the bus, asking with the request's type (or
`text/plain` for text), and leaves out those without `multiple` for a
request of several files.

### Android apps as sources

DeckardSystem's `Chooser` answers `ACTION_CHOOSER` at priority 200, above
the framework's `ChooserActivity` (100). For `ACTION_SEND` or
`ACTION_SEND_MULTIPLE` in `EXTRA_INTENT` it copies the streams to
`xfer/<tid>/` and sends `share_out`; any other chooser starts its intent
with `startActivityAsCaller`, as the app that asked. The sharing app is the one Android recorded as launching the
chooser (`ActivityClient.getLaunchedFromUid/Package`; `getReferrer()` is the
app's to set). The copy reads as the system uid, so each `content:` stream
must pass `checkUriPermission` for the app's uid; others are dropped, as are
`file:` streams.

| `t` | Fields | Meaning |
|---|---|---|
| `share_out` | `id`, `pkg`, `mime`, `title`, `text`, `files` | Android to host. deckardd opens each file in `xfer/<id>/` (regular files only, no links, base names only) and calls `org.sailfishos.share.share(a{sv})` with `resources` (each file as `{name, fileDescriptor}`, then `text` as `{name: title, data}`), `mimeType`, `title` and `source` `waydroid.<pkg>`. |

Two shell changes come with this: the sheet accepts `{name, fileDescriptor}`
resources from any caller (a descriptor is its own proof of access; today
only the paths a Marathon app sees in a shared folder are accepted), and it
honours `source` from session services only, to leave the sharing app out of
the targets. Transfers go with the 30-day cleanup of `xfer/`.

## Permission prompts

Android's runtime permission requests drawn by Marathon's prompt
([../docs/permissions.md](../docs/permissions.md)). PermissionController's
patched GrantPermissionsActivity hands each question to DeckardSystem's
provider `org.marathonos.deckard.system.permissions` (`call("ask")` with a
`ResultReceiver`, `call("cancel")`; PermissionController's uid only), and
DeckardSystem's `BackgroundAccess` asks for "all the time". Both use the
system channel:

| `t` | Direction | Fields | Meaning |
|---|---|---|---|
| `perm_ask` | Android to host | `id` (16 hex digits, DeckardSystem's), `pkg`, `perms` (the Android permissions asked for, 1 to 16), `kind` (`foreground`, `background`, `precise`), `once` (offer "only this time"), `accuracy` (`""`, `choice`, `approximate`), `preciseDefault` | One question. deckardd names `perms` with `PermissionMap::marathon`; all must have the same name. A repeated `id` is ignored. |
| `perm_answer` | host to Android | `id`, `answer` (`allow`, `once`, `deny`, `cancel`, `unsupported`), `precise` | The person's answer. `cancel`: dismissed, as Android's back. `unsupported`: Marathon can't ask (a bad or unnamed request, more than 8 waiting, no shell or an older one); Android shows its own dialog. |
| `perm_cancel` | Android to host | `id` | GrantPermissionsActivity went away; deckardd withdraws the prompt and sends no answer. |

deckardd asks the shell with `org.marathonos.Shell` `/org/marathonos/Shell/Permissions`
`org.marathonos.Shell.Permissions1.PromptAndroid(s appId, s permission,
a{sv} options) → a{sv}`, where `appId` is `waydroid.<pkg>`, `permission` is
the Marathon name and `options` are `request` (= `id`), `kind`, `once`,
`accuracy` and `preciseDefault`. The reply waits for the person (deckardd
waits up to 10 minutes, then answers `cancel`) and is `{answer, precise}`.
`CancelAndroidPrompt(s request)` withdraws one. Both need the caller to be a
declared session service with `android-permissions`. When the system
channel drops, deckardd withdraws every waiting prompt; DeckardSystem then
lets Android ask.

## Settings API

How Marathon's Settings shows and changes Android apps. deckardd exports
`org.marathonos.Deckard1` on `/org/marathonos/Deckard` (bus name
`org.marathonos.Deckard`, session bus); the install methods live beside it as
`org.marathonos.Deckard1.Packages`. Settings lists apps with
`Packages.ListPackages` and removes them with `Packages.UninstallPackage`.

### Callers

`GetRuntime`, `AppIcon` and the signals are open to the session. Every other
method needs the caller to be Settings: deckardd asks the bus for the sender's
pid and the shell's `Navigation1.AppIdForPid` for that pid and up to seven of
its ancestors (`src/callers.cpp`), as the shell resolves its own callers;
the first app id found must be `settings`. Anything else gets
`org.freedesktop.DBus.Error.AccessDenied` and nothing reaches Android.
Arguments are checked first (`InvalidArgs`): package names by the package rule,
permission groups, special-access kinds and battery modes from fixed lists. A
failure in Android is `org.marathonos.Deckard1.Error.Failed` with its message.

### Methods

| Method | |
|---|---|
| `GetRuntime() → a{sv}` | `state` (`stopped`, `starting`, `running`, `stopping`), `memoryBytes` (the container cgroup's `memory.current`, `/sys/fs/cgroup/lxc.payload.waydroid`), `androidVersion`, `uptimeSeconds`, `startAtLogin`, `hasApps` |
| `Start()`, `Stop()`, `Restart()` | `StartUnit`/`StopUnit`/`RestartUnit` of `marathon-waydroid.service` |
| `SetStartAtLogin(b)` | Kept in `~/.local/state/deckard/settings.json`. Unset, it is on when Waydroid has a desktop entry without `NoDisplay=true` (an app the person installed). deckardd starts the session once per login when it is on. |
| `AppIcon(s pkg) → s` | A `data:image/png` URI of `<waydroid data>/icons/<pkg>.png` or deckardd's notification icon, decoded and re-encoded at most 192 px; `""` if none |
| `GetApp(s pkg) → a{sv}` | `pkg`, `label`, `versionName`, `versionCode`, `appBytes`, `dataBytes` (with cache), `cacheBytes`, `notifications`, `links`, `domains` (as), `battery` (`unrestricted`/`optimized`), `permissions` (av of `{name, granted, fixed}`, `name` a Marathon permission; granted if any of its Android permissions is, fixed only if all are) |
| `SetPermission(s pkg, s permission, b granted)` | `permission` a Marathon permission name: grants or revokes its Android runtime permissions that the app asks for, marked user-set (the app may ask again); `notifications` sets Android's notification switch |
| `GetPermissionApps(s permission) → aa{sv}` | The person's apps that ask for it: `{pkg, label, granted, fixed}` (Settings' App permissions) |
| `SetNotifications(s pkg, b)` | Android's per-app setting |
| `SetOpenLinks(s pkg, b)` | Opening the app's verified links in it |
| `SetBattery(s pkg, s mode)` | `unrestricted` adds it to the idle allowlist |
| `ForceStop(s pkg)`, `ClearCache(s pkg)`, `ClearData(s pkg)` | |
| `GetSpecialAccess(s kind) → aa{sv}` | The person's apps that can have it: `{pkg, label, granted}` |
| `SetSpecialAccess(s kind, s pkg, b granted)` | |
| `GetGoogleServices() → a{sv}` | microG (docs/microg.md): `installed`, `enabled`, `push` (microG's cloud messaging; false while microG is off), `accounts` (as, the Google accounts' names, at most 10) |
| `SetGoogleServices(s key, b enabled)` | `enabled`: microG Services and Companion on or off (`setApplicationEnabledSetting`); `push`: its cloud messaging, which connects or disconnects at once |
| `OpenGoogleServices(s page)` | `account`: microG's sign-in (`AccountManager.addAccount("com.google")`); `selfcheck`: microG's self-check. Each opens as an Android window. |
| `ResetAndroid()` | Stops the session, then runs `pkexec /usr/libexec/deckard-reset` (polkit action `org.marathonos.deckard.reset`, allowed for the active session), which removes Android's data so the next start sets Android up again. Replies when the helper is done. |

Signals: `RuntimeChanged(a{sv})` with `GetRuntime`'s map, `AppChanged(s pkg)`
after a change to an app succeeded.

Permissions use Marathon's names (Marathon-Shell
`schemas/marathon-permissions-v1.json`, whose labels and descriptions Settings
and the permission prompts show). `host/deckard-bridge/src/contract.rs` is the
one table from those names to Android's runtime permissions:

| Marathon | Android (`android.permission.`) |
|---|---|
| `location` | `ACCESS_FINE_LOCATION`, `ACCESS_COARSE_LOCATION`, `ACCESS_BACKGROUND_LOCATION` |
| `camera` | `CAMERA` |
| `microphone` | `RECORD_AUDIO` |
| `contacts` | `READ_CONTACTS`, `WRITE_CONTACTS`, `GET_ACCOUNTS` |
| `calendar` | `READ_CALENDAR`, `WRITE_CALENDAR` |
| `storage` | `READ_EXTERNAL_STORAGE`, `WRITE_EXTERNAL_STORAGE`, `READ_MEDIA_IMAGES`, `READ_MEDIA_VIDEO`, `READ_MEDIA_AUDIO` |
| `notifications` | `POST_NOTIFICATIONS` (set through Android's notification switch) |
| `telephony` | `READ_PHONE_STATE`, `READ_PHONE_NUMBERS`, `CALL_PHONE`, `ANSWER_PHONE_CALLS`, `ADD_VOICEMAIL`, `USE_SIP`, `ACCEPT_HANDOVER`, `READ_CALL_LOG`, `WRITE_CALL_LOG`, `PROCESS_OUTGOING_CALLS` |
| `sms` | `SEND_SMS`, `RECEIVE_SMS`, `READ_SMS`, `RECEIVE_WAP_PUSH`, `RECEIVE_MMS` |
| `bluetooth` | `BLUETOOTH_SCAN`, `BLUETOOTH_CONNECT`, `BLUETOOTH_ADVERTISE`, `NEARBY_WIFI_DEVICES`, `UWB_RANGING` |
| `sensors` | `BODY_SENSORS`, `BODY_SENSORS_BACKGROUND`, `ACTIVITY_RECOGNITION` |

Android permissions outside the table are not shown.

Special access:

| Kind | Granted by |
|---|---|
| `all_files` | app op `MANAGE_EXTERNAL_STORAGE` |
| `install_apps` | app op `REQUEST_INSTALL_PACKAGES` |
| `overlay` | app op `SYSTEM_ALERT_WINDOW` |
| `write_settings` | app op `WRITE_SETTINGS` |
| `usage` | app op `GET_USAGE_STATS` |
| `alarms` | app op `SCHEDULE_EXACT_ALARM` |
| `pip` | app op `PICTURE_IN_PICTURE`, apps with a PiP activity |
| `notification_listener` | `NotificationManager.setNotificationListenerAccessGranted` |
| `dnd` | `NotificationManager.setNotificationPolicyAccessGranted` |
| `battery` | the idle allowlist |
| `accessibility` | `Settings.Secure.enabled_accessibility_services` |
| `device_admin` | `DevicePolicyManager.setActiveAdmin` / `removeActiveAdmin` |

Left out: default apps (Marathon answers links, calls, messages and home
through DeckardSystem) and per-app language.

### Management channel

deckardd sends the requests to DeckardSystem on the system channel, which
runs as Android's system uid, so Android's own checks admit them
(`image/apps/DeckardSystem/.../Manage.java`).

| `t` | Fields | |
|---|---|---|
| `manage` (host to Android) | `id`, `op`, then per op | `info`; `app`, `stop`, `cache`, `clear` with `pkg`; `perm` with `pkg`, `permissions` (Android names), `granted`; `perm_apps` with `permissions`; `notif_apps`; `notifications`, `links` with `pkg`, `enabled`; `battery` with `pkg`, `mode`; `special` with `kind`; `special_set` with `kind`, `pkg`, `granted`; `gms`; `gms_set` with `key`, `enabled`; `gms_open` with `page` |
| `managed` (Android to host) | `id`, `ok`, `error` or the answer | `info`: `release`, `sdk`. `app`: `GetApp`'s fields, with `perms` (`{name, granted, fixed}` for each runtime permission the app asks for, by Android name) in place of `permissions`. `special`, `perm_apps`, `notif_apps`: `apps`. `gms`: `installed`, `enabled`, `push`, `accounts`. |

DeckardSystem refuses every per-app op on packages with `FLAG_SYSTEM`, so
only apps the person installed are changed; the `gms` ops act on microG
alone (`GoogleServices.java`). deckardd answers a request with
an error after 30 s without a reply or when the channel drops, keeps at most
32 waiting, and cleans each answer (`ManageCheck`, `tests/test_settingsapi.cpp`):
labels at most 128 characters, sizes 0 to 2^50, only known groups and kinds,
at most 50 well-formed domains and 500 apps.

### Reset

`host/packaging/deckard-reset`, root, through pkexec only (it takes the user
from `PKEXEC_UID` and refuses root). It removes everything in that user's
`~/.local/share/waydroid/data` (Android's `/data`) except `deckard/`, where
deckardd's sockets live, and removes `deckard/xfer` and `deckard/install`
and Waydroid's `waydroid.*.desktop` entries. Waydroid's images in
`/var/lib/waydroid` and Deckard's overlay stay. It stops
`waydroid-container.service` first and touches nothing if that fails, and
starts it again only if it was running. It works from inside the data
directory after checking it is in the user's home and owned by them, so a
symlink can't send the removal elsewhere. Exit status: 0 done, 2 usage, 3 no
data, 1 failure. `tests/test-deckard-reset.sh` runs it against a fake home,
passwd and systemctl.
