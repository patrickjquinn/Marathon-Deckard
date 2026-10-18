# Deckard bridge protocol

The system bridge between Android (inside the Waydroid container) and
Marathon. On the Android side a privileged app, DeckardBridge
(`image/apps/DeckardBridge`, package `org.marathonos.deckard.bridge`), listens
to notifications and media sessions. On the host, `deckardd`
(`host/deckardd`) posts them to Marathon's Hub (`org.freedesktop.Notifications`)
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

Everything from the container is untrusted. deckardd bounds and cleans every
field before it reaches a Marathon service (`host/deckardd/src/validate.cpp`):

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

## IME channel

Marathon's keyboard in Android apps. DeckardIme (`image/apps/DeckardIme`,
package `org.marathonos.deckard.ime`, `/system/app` in the overlay) is
Android's only input method; it draws nothing. deckardd's `Ime`
(`host/deckardd/src/ime.cpp`) relays between it and the shell's
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
| `Register(s prefix)` | deckardd at start and whenever the shell reappears, with `waydroid.`. Needs the caller to be a declared session service (`/usr/share/marathon-shell/services/deckardd.service.json`, permission `text-input`); the last caller owns the channel. |
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
