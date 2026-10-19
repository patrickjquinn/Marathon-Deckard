# End-to-end verification, 19 October 2026

One pass over every Deckard feature from the user's side, on the HackberryPi
CM5 (720×720, hardware keyboard), shell e7b5f3f1, deckard f0b0c64, Waydroid
1.6.3 with LineageOS 20 VANILLA (20260927). 05:21–06:33 IST. Driven with taps
(`/run/touch.fifo`, screenshot coordinates = tap coordinates), the shell's
D-Bus API and `keel-mcp --stdio`, and checked against logcat, `dumpsys`,
`busctl monitor` and the host journal. DND stayed on, Bluetooth stayed off, and
nothing played audio. No code was changed.

Screenshots are contact sheets in [verification-2026-10-19/](verification-2026-10-19/).

## Result

| # | Feature | Result | Evidence |
|---|---|---|---|
| 1.1 | First launch from the grid starts Android and opens the app | **FAIL** (2 of 2) | [cold-start](verification-2026-10-19/cold-start.png), F1 |
| 1.2 | Launch from the grid with Android running | PASS: Feeder's window after 2.9 s, Page after 1.4 s | [cold-start](verification-2026-10-19/cold-start.png) last tile |
| 1.3 | Start at login / follows the session | PASS: on in Settings; after the greetd restart the session stopped and started again by itself and Android was ready 11 s later | journal 06:27:32–06:27:44 |
| 1.4 | Settings › Android apps: status, version, memory, uptime, Stop, Start | PASS: Stop → Stopped, Start → `sys.boot_completed` 10.8 s after the tap. The memory row says "MB" but shows MiB (870.2 "MB" = 912,842,752 B) | s03–s19 |
| 1.5 | Boot time, 3 session restarts | PASS: 10.9 s, 10.9 s, 10.9 s (session start to `sys.boot_completed=1`), never frozen | `restart` runs 05:29, 05:32, 05:34 |
| 1.6 | Composer aborts | PASS on boot: 0 in 7 boots (05:24–06:27); the patched composer (`42bef881…`) was loaded in each of the 3 timed boots. 1 SIGABRT when the shell exited for the greetd restart (06:27:29), the connection-end abort of composer-crash.md §2 | `coredumpctl` |
| 1.7 | Memory cost | Measured: host MemAvailable 304–309 MiB below the stopped state 2 minutes after boot (slim.md recorded 246 MiB); container `memory.current` 861–873 MiB | table below |
| 2.1 | Store: search F-Droid, Get, grid, own window | PASS: "page" lists F-Droid apps with icons; Page Get → Installing → Open + "Page installed" in about 5 s, installer `org.marathonos.deckard.bridge`, grid entry, opens in its own window | [store](verification-2026-10-19/store.png) |
| 2.2 | Install from an .apk (`deckard-open-apk`) | PASS, with a UI fault: the install page is right and the app installs, but afterwards the page has no button and no notice | [store](verification-2026-10-19/store.png) F7 |
| 2.3 | Update from an .apk | PASS, with UI faults: "Update app, replacing 0.1.0", `lastUpdateTime` 05:50:30 → 05:51:24; the Keel call returned an error and the button vanished again afterwards | F7 |
| 2.4 | Uninstall from the Store | PASS: pulley › Uninstall, remorse, package and desktop entry gone, button back to Get | [store](verification-2026-10-19/store.png) |
| 2.5 | Uninstall from Settings | PASS, with a false error: removed (package, desktop entry, list 7 → 6), but Settings shows "Couldn't do that: not installed" | [store](verification-2026-10-19/store.png) last tile, F8 |
| 3.1 | Own switcher card per app, live thumbnail | PASS: Markor, Feeder, Notify Test, Catima, FOSS Browser cards with live content next to Marathon's | [windows](verification-2026-10-19/windows.png) |
| 3.2 | No Android chrome, fills the window | PASS: no caption, status or navigation bar; every app fills the area between Marathon's status bar and nav bar | [windows](verification-2026-10-19/windows.png) |
| 3.3 | Marathon colours, dark, fonts | PASS with notes: Source Sans 3 in every app; Feeder and Catima dark with the teal accent. Notify Test (no theme of its own) renders light although `cmd uimode night` = yes; Catima keeps its own brown surfaces; FOSS Browser its own blue | [windows](verification-2026-10-19/windows.png) |
| 3.4 | Taps across 3 Android apps + 1 Marathon app | PASS: Markor intro → page 2, Feeder hamburger → drawer, Notes, Notify Test, switching through the switcher each time; every tap landed in the front app | [windows](verification-2026-10-19/windows.png) |
| 3.5 | Back gesture | PASS in 3 apps: Feeder drawer closes and article → list, Markor intro page 2 → page 1, Notify Test root logs `KEYCODE_BACK` and Marathon shows the grid | [windows](verification-2026-10-19/windows.png) |
| 3.6 | Hardware typing into an Android field | PASS: "Hello hw 2" in Page, with Shift and Backspace | [keyboard](verification-2026-10-19/keyboard.png) |
| 3.7 | On-screen keyboard typing (TextInput1 `CommitString`) | **FAIL** (Page and FOSS Browser) | [keyboard](verification-2026-10-19/keyboard.png), F2 |
| 3.8 | Home takes the keyboard back | PASS: first swipe up → `HidePanel`, Android `mInputShown=false`, app stays; second → switcher | [keyboard](verification-2026-10-19/keyboard.png) |
| 3.9 | Secure window blanked on its card | NOT TESTED: none of the installed apps sets `FLAG_SECURE` | |
| 3.10 | Closing an Android card | **FAIL** (intermittent) | [robustness](verification-2026-10-19/robustness.png) first tile, F9 |
| 4.1 | Notification with icon and name | PASS: `app_name` "Notify Test", `file://` icon, `desktop-entry`, Hub chip "Notify Test 2" | [hub-media](verification-2026-10-19/hub-media.png) |
| 4.2 | Actions | PASS: "First" → `event first` in the app, row closed | [hub-media](verification-2026-10-19/hub-media.png) |
| 4.3 | Inline reply from the Hub row | **FAIL**: the row shows only "Mark read"; the `inline-reply` action reaches the shell but the Hub row has no reply field | [hub-media](verification-2026-10-19/hub-media.png), F5 |
| 4.4 | Tap a notification | **FAIL**: the app's `contentIntent` runs in Android, but Marathon stays on the Hub | F4 |
| 4.5 | Dismiss sync both ways | PASS: swipe in the Hub → `NotificationClosed` reason 2 and the Android record is gone; `cancelAll()` in the app → `CloseNotification` and the row is gone | |
| 4.6 | Toast as a Marathon app toast | **FAIL** (regression) | F3 |
| 4.7 | Media session in the QS media card (silent) | PASS: MPRIS player with metadata; card "Silent track 1 / Deckard"; next → `media next` in the app, card on track 2; stopping the session removes the player | [hub-media](verification-2026-10-19/hub-media.png) |
| 5.1 | https link → Marathon browser | PASS (example.org) | [handoffs](verification-2026-10-19/handoffs.png) |
| 5.2 | tel: → Phone | PASS: dialer with +1 555 123 4567 | [handoffs](verification-2026-10-19/handoffs.png) |
| 5.3 | sms: with a body → Messages draft | PASS: conversation with "Verify body 0609" in the field | [handoffs](verification-2026-10-19/handoffs.png) |
| 5.4 | geo:0,0?q= → Maps search | PASS: Maps search "coffee" with results | [handoffs](verification-2026-10-19/handoffs.png) |
| 5.5 | Camera capture → Marathon Camera → photo back | PARTIAL: capture mode opens ("Take a photo to send it back"), then "Camera unavailable"; the HackberryPi has no camera, so the return path is untested | [handoffs](verification-2026-10-19/handoffs.png) |
| 5.6 | File picker → Marathon chooser → file reaches the app | **FAIL**: the sheet works and the file is copied, but the app may not open it and Catima crashed | [files-share](verification-2026-10-19/files-share.png), F6 |
| 5.7 | Android share → Marathon sheet → Marathon target | PASS: Feeder › Share → Marathon's sheet with the URL, Marathon and Android targets, Feeder not listed; New note → a note with the URL | [files-share](verification-2026-10-19/files-share.png) |
| 5.8 | Marathon share sheet → Android target | **FAIL**: Gallery › Share lists Catima, Save as and Markor; Markor starts in front but cannot read the image | [files-share](verification-2026-10-19/files-share.png), F6 |
| 6.1 | Per-app permissions | PASS: Markor Files on → READ/WRITE_EXTERNAL_STORAGE granted, `USER_SET`; off → revoked | [settings-pilot](verification-2026-10-19/settings-pilot.png) |
| 6.2 | Notifications switch | PASS: Notify Test → `POST_NOTIFICATIONS` granted, `USER_SET` | [hub-media](verification-2026-10-19/hub-media.png) |
| 6.3 | Battery | PASS: Unrestricted → on the deviceidle allowlist; Optimized → off | |
| 6.4 | Clear cache | PASS: 244 kB → 16 kB | [settings-pilot](verification-2026-10-19/settings-pilot.png) |
| 6.5 | Force stop | PASS: `Force stopping net.gsantner.markor … from pid 735` (DeckardSystem), process gone | |
| 6.6 | Special access | PASS: 12 kinds listed; All files › Material Files on → `MANAGE_EXTERNAL_STORAGE: allow`, off → deny | [settings-pilot](verification-2026-10-19/settings-pilot.png) |
| 6.7 | Settings deep links from apps | PARTIAL: `APPLICATION_DETAILS_SETTINGS` → Marathon Settings' app page (PASS). Material Files' "all files access" request opens **Android's** All files access page inside the app's window | [settings-pilot](verification-2026-10-19/settings-pilot.png), F10 |
| 7.1 | keel-mcp lists Android apps | PASS: 23 apps, 125 tools; Settings, Feeder, FOSS Browser, Catima, Material Files and Markor with `app.show`, `screen.read`, `share.text`, `ui.*` and shortcuts. Notify Test is skipped by design (`Actions.launchable()` leaves out `org.marathonos.deckard.*`) | |
| 7.2 | Invoke a shortcut | PASS: `shortcut.downloads` (Material Files) → Done, Files opens in Download | [settings-pilot](verification-2026-10-19/settings-pilot.png) |
| 7.3 | screen.read | PASS: Material Files' dialog text, labelled untrusted. When the app's task showed Android's Settings activity, it answered `NotAvailable: Android cannot say whether the screen is protected` (fails closed) | |
| 7.4 | ExpectAppWindow path (share.text into a cold app) | PASS: Markor force-stopped; `share.text` → Done; deckardd called `ExpectAppWindow`, the shell sent `ForegroundAppChanged`, Markor came to the front | [settings-pilot](verification-2026-10-19/settings-pilot.png) last tile |
| 7.5 | Pilot itself | NOT TESTED: Pilot needs onboarding consent, which I did not give | |
| 8.1 | greetd restart: Android follows, apps come back | PASS with a note: the session stopped (06:27:32) and started again by itself (06:27:33), and Android was ready at 06:27:44; Android's DND stayed on (alarms only). No card came back, Android or Marathon (Android keeps the tasks without windows); Markor relaunched from the grid in 1.4 s | [robustness](verification-2026-10-19/robustness.png) |
| 8.2 | Shell memory over 10 open/close cycles | PASS: Pss 258,591 → 261,473 kB (+2.9 MB, peak 263,914 at cycle 8, 261,537 after 30 s); threads 13, fds 240 → 235 | table below |
| 9 | Marathon sanity | PASS: grid, switcher, Hub, QS, Settings, Store, Notes, Gallery, Browser, Phone, Messages, Maps, Camera, Clock and Calculator all opened and drew normally, before and after the restart | [robustness](verification-2026-10-19/robustness.png) |

## Failures

### F1. The first launch from the grid never opens the app

Repro (twice: Markor 05:24, Catima 05:39):
1. Stop Android: Settings › Android apps › pull down › Stop Android (or
   `systemctl --user stop marathon-waydroid`).
2. Tap an Android app on the grid.

Expected: the app's window once Android has booted (about 12 s).

Seen: the session starts at once (Markor: 05:24:49.86) and Android is ready at
+11.2 s ("Android with user 0 is ready"), but the app is never started: there
is no `am_proc_start` or `START` for the package in logcat. At +22.4 s the
shell logs `AppWindow: Launch timeout for waydroid.net.gsantner.markor — no
surface after 20 s` and shows "Markor couldn't open". The same happened with
Catima (start 05:39:59, ready 05:40:11, timeout 05:40:22).

Then "Try again" starts the activity in Android (`topResumedActivity` is the
app) and the shell's `foregroundApp` becomes the app, but the screen (shell
grab and kmsgrab scanout alike) shows only the wallpaper, with no app and no
grid, for over a minute. The app appears only when the user opens the
switcher and taps its card.

Likely cause, not confirmed: `AppLaunchService::startAndroidSession`
(shell/src/services/applaunchservice.cpp) runs `waydroid app launch` as soon
as `id.waydro.Session` registers, which is at container start, before Android
has booted, and the launch is dropped.

### F2. On-screen keyboard typing does not reach Android apps

Repro: open Page (or FOSS Browser), tap its text field, open Marathon's
keyboard with the nav bar's keyboard button, type.

Seen on `busctl monitor` (TextInput1): `SetEditor(waydroid.com.page.bizzle, 0,
515)`, `SetSurroundingText("Hello hw 2", 10, 0)`, `ShowPanel`, and
`PanelChanged(355, 628)` when the keyboard opens. Android's IME inset follows
(the browser's URL dialog moved above the keyboard). Typing "d", "i", space
gives **no `CommitString`** and the field does not change. In the browser's URL
field (purpose 5) the keyboard shows word predictions ("di", "dizzy",
"dizziness"), so it is not using the editor's purpose either. Tapping the
Android field while the keyboard is up sends `HidePanel` and closes it.
Hardware typing into the same fields works. keyboard.md recorded the same
failure on 2edb21fd and a fix in b25a8096; it is still failing on e7b5f3f1.

### F3. Android toasts never appear (regression from slim S3)

Repro: `am broadcast -a org.marathonos.deckard.notifytest.POST -n
org.marathonos.deckard.notifytest/.TestReceiver --es kind toast --es text
'Verify toast'`, with Notify Test in front (06:07) or in the background (06:06).

Seen: the app logs `post toast`; ToastWatcher is enabled and bound
(`dumpsys accessibility`); no `Notify` call with `x-marathon-toast` reaches the
shell and nothing is drawn. Text toasts of apps targeting Android 11+ are
drawn by SystemUI through `IStatusBar.showToast`. Slim stage S3 removed
SystemUI (no `com.android.systemui` package; slim.md's SystemUI table notes
these toasts are lost), so `ToastPresenter` never runs and ToastWatcher gets no
event. bridge.md's toast check passed before S3.

### F4. Tapping an Android notification doesn't show the app

Repro: post the chat notification (`--es kind chat`), open the Hub, tap the row.

Seen: Android logs `activity opened from chat` and the auto-cancel removes the
row, but Marathon stays on the Hub and the foreground app is empty. The shell
logs `[NotificationHandler] No installed app for notification from
"waydroid.org.marathonos.deckard.notifytest"`
(`NotificationHandlerCpp::navigateToNotificationTarget`,
`NotificationTarget::resolve` finds no app for the `waydroid.` id).

### F5. No inline reply in the Hub row

The chat notification carries `inline-reply` with "Reply" and the reply
placeholder, but the Hub row shows only "Mark read". bridge.md noted that
only the banner has a reply field; banners are suppressed while DND is on, so
there was no way to reply here.

### F6. Files handed to Android apps can't be opened (file picker and share)

Repro A (file picker): Catima › menu › Import/export › From filesystem ›
Catima › OK. Marathon's chooser opens; pick a file in Documents › Open.

Repro B (share): Gallery › a photo › pull down › Share › Markor.

Seen in both: deckardd copies the file into `/data/deckard/xfer/<id>/` and
DeckardSystem hands the app
`content://org.marathonos.deckard.system.files/document/<id>%2F<name>`.
system_server refuses the grant:

```
UriGrantsManagerService: For security reasons, the system cannot issue a Uri permission grant to content://org.marathonos.deckard.system.files/document/…; use startActivityAsCaller() instead
ContentProviderHelper: Permission Denial: opening provider org.marathonos.deckard.system.Files from ProcessRecord{… me.hackerchick.catima/u0a37} … requires that you obtain access using ACTION_OPEN_DOCUMENT or related APIs
```

Catima then crashes with that SecurityException (its own crash dialog).
Markor starts in front (`START … SEND typ=image/*`) but can't read the
image. DeckardSystem runs as the system uid, and Android refuses URI grants
handed out directly by the system uid (`Pick.java` `setResult` with
`FLAG_GRANT_READ_URI_PERMISSION`; `Share.java:140` `startActivity`). slim.md's
S4 check read the URI with `content read` from the shell, which needs no
grant, so it didn't show this.

### F7. The Store's install page after installing from a file

Repro: `deckard-open-apk ~/Downloads/<app>.apk`, Install.

Seen: the app installs (05:50:30, installer DeckardBridge, grid entry), but the
page then shows neither a button nor the "installed" notice. Opening the
file again shows "Update app, replacing 0.1.0" correctly, but the Keel call
returns an error: `the handler failed:
qrc:/qt/qml/MarathonApp/Store/components/GetButton.qml:57:9: Unable to assign
[undefined] to QString`. After Update (05:51:24) the button vanishes again.

### F8. Uninstall from Settings reports a false error

Repro: Settings › Android apps › Page › pull down › Uninstall, let the remorse
timer run out. The app is removed, then Settings shows "Couldn't do that: not
installed" over the refreshed list.

### F9. Closing Android cards sometimes fails

Repro: 10 cycles of `LaunchApp waydroid.<pkg>` (Feeder, Catima, FOSS Browser in
turn), switcher, the card's ✕.

Seen: the shell logged `[WaylandCompositor] closeWindow called for unknown
surface ID` three times (06:24:05 id 40, 06:24:41 id 94, 06:25:16 id 100).
After the tenth cycle a black card stayed top-left with the labels "FOSS
Browser" and "Feeder" drawn over each other. Its ✕ did nothing (another
`unknown surface ID: 100`), Android still had Feeder's window, and tapping the
card showed FOSS Browser.

### F10. "All files access" opens Android's Settings page

Repro: Material Files (no grant yet) › its dialog "App needs access to manage
all files" › OK.

Seen: `com.android.settings/.Settings$AppManageExternalStorageActivity` opens
inside Material Files' window, an Android-styled page, not Marathon's
Special access › All files page (which works when opened from Settings).

## Other observations

- A freshly installed app that targets Android 13 has no notification
  permission until it asks for it. Notify Test never asks, so its first
  notifications were dropped until I turned Notifications on in Settings.
  Material Files asked, which showed Android's permission dialog inside its
  window.
- system_server logs `storaged not found; trying again` every second
  (storaged is stopped by slim E3).
- Store search: F-Droid results come after all Flathub results ("page"); a
  search for a package-id fragment ("farmerbb") found nothing and said
  "Nothing on Flathub".
- deckardd logs nothing on success, so its re-reads after the shell restart
  could not be confirmed from the journal (only the two failures during the
  gap: `cannot read the ambience`, `cannot read Marathon's audio settings`).

## Measurements

Boot and memory (`restart` script: session stop, 30 s, `MemAvailable`, start,
poll `sys.boot_completed`, 120 s, measure):

| Start | Boot | Frozen polls | MemAvailable stopped → running | Δ | `memory.current` | Cores |
|---|---|---|---|---|---|---|
| 05:29:21 | 10.9 s | 0 | 14,145,044 → 13,828,256 kB | 309 MiB | 903,475,200 | 0 |
| 05:32:03 | 10.9 s | 0 | 14,143,072 → 13,831,268 kB | 304 MiB | 903,278,592 | 0 |
| 05:34:45 | 10.9 s | 0 | 14,141,472 → 13,830,576 kB | 304 MiB | 915,582,976 | 0 |

Shell (`/proc/<pid>/smaps_rollup` of marathon-shell-bin) over 10 Android
open/close cycles:

| | Rss | Pss | Pss_Anon |
|---|---|---|---|
| before | 370,492 kB | 258,591 kB | 226,540 kB |
| after cycle 5 | 373,204 kB | 261,356 kB | |
| after cycle 8 (peak) | 375,532 kB | 263,914 kB | |
| after cycle 10 | 373,320 kB | 261,473 kB | |
| 30 s later | 373,704 kB | 261,537 kB | 229,472 kB |

## Cleanup

Page was installed and removed again. Notify Test is installed (kept, with
notifications on). The grants I changed were put back: Markor's storage
permission and its `USER_SET` flags, Material Files' `MANAGE_EXTERNAL_STORAGE`
(back to default), Markor's battery setting. The test note, picture, document,
APK copy and the two `xfer/` folders were deleted. Left as found: Material
Files waiting on Android's notification prompt; Catima after its crash; Markor's
cache cleared.
