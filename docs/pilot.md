# Pilot and Android apps

Pilot, Shipwright's assistant, works with Marathon apps through Keel Actions.
keel-mcp serves every app's actions as MCP tools, and pilotd calls them. Deckard
makes each Android app a Keel Actions provider, so Pilot can use it as Gemini
uses apps on Android: open the app or a link in it, run its launcher shortcuts,
share text to it, search in it, read its screen, and tap and type there.
Pilot and Keel are unchanged. The wire protocol, the manifest and the limits
are in [../protocol/bridge.md](../protocol/bridge.md), section *Keel Actions*.

## How keel-mcp finds an app

These points come from reading Shipwright `keel/actions/mcp` (`catalogue.rs`,
`backend.rs`, `server.rs`). That code is the same as the
shipwright-open export e37941bc that the `keel-mcp` aport builds.

- keel-mcp reads installed manifests from `/usr/share/keel/actions/*.json`.
  Every 10 s it also lists the session bus, calls `Describe()` on each
  well-known name, and keeps the manifests whose `appId` equals the name
  (500 ms timeout). An installed manifest wins.
- A name is described again only when its owner (its unique name)
  changes.
- `Invoke(action, json, {userInitiated})` goes to the bus name, always at
  `/org/shipwright/Keel/Actions`. The call carries only the bare action
  name, so the bus name is the only thing that says which app is meant.
  An app is a bus name, not a desktop entry.
- `userInitiated` is passed on only from a trusted client (pilotd, by
  executable). Destructive and `confirm` tools get `NeedsConfirmation` for
  any other client. Pilot asks the person before destructive, `confirm` and
  open-world tools.
- The context resource `keel://<app>/context` is read only for a trusted
  client with `userInitiated`, and never for `--external`.

So deckardd owns `waydroid.<pkg>`, the shell's app id for the app, once per
app on a bus connection of its own. Its `Describe` answers that app's
manifest. A changed manifest (a new dynamic shortcut, an update) moves to a
new connection, so the name gets a new owner and keel-mcp describes it again.

## Privacy

- **Only keel-mcp and pilotd are served.** `Invoke` and `GetContext` are
  served only to callers whose `/proc/<pid>/exe` is `/usr/bin/keel-mcp` or
  `/usr/bin/pilotd`, compared by device and inode as Keel's
  `KeelCallerTrust` does. A Marathon app calling deckardd directly gets
  `AccessDenied`.
- **Only the app in front.** `screen.read` and the context answer only when
  the shell's `shell.state` says the screen is on, the phone is unlocked and
  `foregroundApp` is that app. Android reads only that package's
  `TYPE_APPLICATION` window: no other app, and not the keyboard or system
  windows.
- **No passwords.** Android never sends `isPassword()` text, and deckardd
  drops it again if it does. A password field is rendered as
  `field password (hidden)`, and `ui.type` refuses one.
- **No paired computers.** A keel-mcp started with `--external` gets no
  screen content.
- **Untrusted.** `screen.read` is marked `untrusted`. keel-mcp then labels
  the result as data and not instructions, and Pilot keeps it apart from
  the person's request: an app's screen can contain text written to steer a
  model.
- **Acting needs a person.** `ui.tap` and `ui.type` are `confirm` and
  destructive. keel-mcp gives them only to Pilot, Pilot shows its card for
  each one, and deckardd also requires `userInitiated`. Shortcuts whose
  label acts at once (call, send, pay, delete, record...) are `confirm` too.
- **On demand, never streaming.** The `ScreenReader` accessibility service
  subscribes to no events. It is enabled only when deckardd asks for a read
  or an action, and disabled two minutes after the last one, so no app
  builds an accessibility tree for it the rest of the time.
- **Never a secure window.** Accessibility does not report `FLAG_SECURE`,
  so DeckardSystem checks the window manager's dump (the system uid holds
  `DUMP`) before every read. A secure window of the app (a banking app, a
  password manager) is refused. So is any read DeckardSystem cannot vouch
  for: it is not connected, or it finds no window of the app.

## Decisions

- **One provider per app, not one with an app argument.** Pilot first picks
  a domain (an app) from tool titles, then picks a tool. Android apps then
  appear as apps beside Marathon's. Titles carry the app's label
  ("Open F-Droid", "New message (Signal)"), because Pilot names a domain
  without a desktop entry from its id alone (`app_name` falls back to the
  last segment of the id).
- **Android starts the activity; the shell expects its window.** An
  Android window that nobody launched from Marathon is filed as a card
  (b05f45a9, c05a728d). Before `launch`, deckardd calls
  `Navigation1.ExpectAppWindow(appId)`, a call proposed to the shell. With
  it, the shell shows the app at once, and a window of a new task (an
  activity started with its own affinity, or a document) takes the front
  as a launch would. Without it, deckardd waits for `apps.running` to list
  the app and calls `app.launch`, which shows the window the shell already
  has, but not one of a new task. Launching through the shell first would
  start `waydroid app launch`'s launcher intent alongside the action's, and
  with `NEW_TASK` the launcher activity can land on top of a deep link.
- **Links come from the app's own manifest.** Android has no public API
  for another app's intent filters. Actions reads the compiled
  `AndroidManifest.xml` through `AssetManager.openXmlResourceParser`, using
  the cookie whose manifest names the package. It collects the
  `VIEW` + `BROWSABLE` filters of exported, enabled activities. Share and
  search use `queryIntentActivities(..., MATCH_DEFAULT_ONLY)`, exactly what
  `startActivity` will resolve.
- **Search tries the assistant's intent first.**
  `com.google.android.gms.actions.SEARCH_ACTION` is what apps declare for
  Google's assistant ("search for X in Y"), with `ACTION_SEARCH` as the
  fallback.

## Shortcuts come from DeckardSystem

`LauncherApps.getShortcuts` and `startShortcut` need the caller to be a
shortcut host: the default launcher, the assistant, or a holder of
`ACCESS_SHORTCUTS`. In this image's `framework-res.apk` that permission's
protection level is `0x6000002`, that is `signature|role|recents`, not
`privileged`. DeckardBridge is signed with Deckard's key, so it is not a
host. DeckardSystem runs as `android.uid.system`, which holds every
permission. So DeckardSystem's `AppActions` answers deckardd's shortcut
queries and starts shortcuts, over the system channel. It returns the
manifest and the dynamic shortcuts, so recent chats and "Navigate home"
come too. `startShortcut` starts a shortcut as the app that published it,
so non-exported targets work as they do from a launcher.
`onShortcutsChanged` tells deckardd to ask again. DeckardBridge is not
re-signed.

## Pilot's on-screen context

pilotd's `ShowSheetFor(pid)` finds the app over which Pilot was opened from
the window's process. Marathon's patch 0003 (`packaging/packages/shoal-pilotd`
in Marathon-Shell) asks the shell `Navigation1.AppIdForPid` for a
`marathon-app-runner`. An Android window belongs to Waydroid's process, so
for any other process the patch now asks `Navigation1.GetForegroundApp()`
(being added to the shell). When the answer is a `waydroid.<pkg>` id, Pilot
takes it as the app on screen and names it from Waydroid's desktop entry
(`$XDG_DATA_HOME/applications/waydroid.<pkg>.desktop`). Pilot then reads
`keel://waydroid.<pkg>/context`, which deckardd serves (`GetContext`: the
screen text, at most 4,000 characters, or nothing for a secure window), and
puts that app's tools first, as it does for Marathon apps.

## Verified

- On the host (2026-10-19): `test_keelapps`, which covers cleaning,
  manifests, link rules and rendering. It also runs a round trip on a
  private `dbus-daemon` with a fake shell, a fake bridge and a fake
  DeckardSystem:
  - shortcuts from DeckardSystem, with a new owner for the new list
  - Describe and app.show
  - a shortcut started by DeckardSystem, then apps.running → app.launch
  - a refused share
  - a search with a shell that expects the window
  - a secure window refused, then screen.read
  - the confirm path of ui.tap, and GetContext
  - no read without DeckardSystem, and keel_changed
  - the lock, and losing Android
  - the restart from `apps.json`, and refusing an untrusted caller
- In the Alpine build (`deckard` aport against this repo's HEAD): all 9
  ctest suites pass, test_keelapps included.
- DeckardBridge and DeckardSystem build with `build-apk.sh` (javac, d8,
  aapt; DeckardSystem with the platform key). On the host JVM, the
  secure-window parser matched a secure window, a plain one and one
  without a surface. It also refused a package-name prefix. The sample is
  Android 13's `dumpsys window windows` format, still to be checked against
  the device's.
- pilotd: `the_shell_names_the_android_app_in_front` and the existing
  Marathon adapter tests pass (`cargo test -p shoal-pilotd --test marathon`).
- On the HackberryPi (2026-10-19 03:18–03:26). The shell had
  `GetForegroundApp`, `ExpectAppWindow` and `SetAppWindowSecure`, and
  Deckard was at e3df5d5. Android's data had just been reset, so the only
  app was Android's Settings. I drove everything through `keel-mcp --stdio`
  over ssh:
  - **Discovery.** keel-mcp lists `waydroid.com.android.settings` with
    `app.show`, `screen.read`, `ui.tap`, `ui.type` and three shortcuts that
    DeckardSystem returned: `manifest-shortcut-wifi`, `-data-usage` and
    `-battery`, with ids in LauncherApps' form. A new keel-mcp lists them
    once its first probe has run (within 10 s), not in its first
    `tools/list`.
  - **A shortcut.** `shortcut.manifest_shortcut_battery` answered Done.
    DeckardSystem started it, and its own stub then sent Android's battery
    page to Marathon Settings (`foregroundApp` `settings`,
    [settings-battery-shortcut.png](pilot/settings-battery-shortcut.png)).
  - **app.show** of Android Settings: deckardd's call succeeded and
    `foregroundApp` became `waydroid.com.android.settings`. Android
    Settings itself never mapped a window: the shell showed "Settings
    couldn't open". That is outside Keel (slim stages). `screen.read` then
    answered `NotAvailable: Android cannot say whether the screen is
    protected`, because the app had no window to check: it failed closed,
    as intended.
  - **Install → publish → withdraw.** `pm install` of Markor (F-Droid)
    published `waydroid.net.gsantner.markor` (app.show, share.text,
    screen.read, ui.*) within seconds, with no restart. `pm uninstall`
    withdrew the bus name and the `apps.json` entry.
  - **share.text** into Markor, a cold start: Done. The shell expected the
    window, and Markor's activity came to the front as a launch would
    ([markor-share.png](pilot/markor-share.png), showing Markor's own
    storage permission prompt).
  - **screen.read** of Markor: DeckardSystem's secure check found Markor's
    window not secure, and ScreenReader was bound on demand. The result,
    marked untrusted:
    `[1.0] text "Storage permission required to read and write files" #message`,
    `[1.1] button "EXIT" #button2`, `[1.2] button "OK" #button1`. A
    second read, with the service bound again from cold, took about 0.4 s
    including keel-mcp.
  - **On demand.** ScreenReader appeared in `enabled_accessibility_services`
    for a read. It was gone again two minutes later (only ToastWatcher
    left), and came back for the next read.
  - **ui.tap from a non-Pilot client:** `NeedsConfirmation ... only Pilot,
    which asks the person, can confirm them`.
  - **The secure parser** on the device's real `dumpsys window windows`:
    Markor not secure. The same dump with `SECURE` added to Markor's `fl=`:
    secure. Settings, with no window: refused.
  - **Pilot** was running, but `Ask` answered route `not-onboarded`.
    Nothing reaches a model until the person consents in Pilot, and I did
    not consent for them. So `ui.tap`, `ui.type` and the context read
    through Pilot are untested on the device. pilotd there is r2, without
    patch 0003's Android mapping.
  - **Not on the device yet:** deckardd telling the shell about secure
    windows (`SetAppWindowSecure`; tested on the host).
