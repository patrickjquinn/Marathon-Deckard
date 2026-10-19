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
- **Not covered: `FLAG_SECURE`.** Accessibility does not report it, so a
  secure window (a banking app) is read like any other when it is in front.

## Decisions

- **One provider per app, not one with an app argument.** Pilot first picks
  a domain (an app) from tool titles, then picks a tool. Android apps then
  appear as apps beside Marathon's. Titles carry the app's label
  ("Open F-Droid", "New message (Signal)"), because Pilot names a domain
  without a desktop entry from its id alone (`app_name` falls back to the
  last segment of the id).
- **Android starts the activity, then the shell shows it.** An Android
  window that nobody launched from Marathon is filed as a card (b05f45a9,
  c05a728d). deckardd sends `launch`, waits for `apps.running` to list the
  app, and calls the shell's `app.launch`, which shows the window the
  shell already has. Calling `app.launch` first would start
  `waydroid app launch`'s launcher intent alongside the action's, and with
  `NEW_TASK` the launcher activity can land on top of a deep link.
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

## Shortcuts: what the bridge may see

`LauncherApps.getShortcuts` and `startShortcut` need the caller to be a
shortcut host: the default launcher, the assistant, or a holder of
`ACCESS_SHORTCUTS`. In this image's `framework-res.apk` that permission's
protection level is `0x6000002`, that is `signature|role|recents`, not
`privileged`. DeckardBridge is signed with Deckard's key, so it is not a
host. The only host on the device is DeckardHome, as the HOME role holder.

- **Today:** shortcuts come from each app's `android.app.shortcuts` XML. These
  are the manifest shortcuts, for example a messenger's "New message".
  Actions starts the shortcut's intents itself, so a shortcut whose target
  activity is not exported cannot start. Dynamic shortcuts (recent chats,
  "Navigate home") are not seen.
- **Full shortcuts:** the manifest asks for `ACCESS_SHORTCUTS`, and Actions
  uses `LauncherApps` (manifest and dynamic, `onShortcutsChanged`) whenever
  `hasShortcutHostPermission()` is true. Two ways to get there:
  1. Sign DeckardBridge with the platform key, as DeckardSystem is
     (`PLATFORM_KEY=... image/tools/build-apk.sh`). This changes the key of
     an installed system app, which Android accepts for system-partition
     packages, but it has to be checked on the device and agreed with the
     bridge's owner.
  2. Ask DeckardSystem (`android.uid.system`) for the shortcuts over the
     system channel.

## Pilot's on-screen context

pilotd's `ShowSheetFor(pid)` finds the app over which Pilot was opened from
the window's process. Marathon's patch 0003 asks the shell
(`Navigation1.AppIdForPid`) only for a `marathon-app-runner`. For an Android
window the process is Waydroid's, so Pilot does not know which app is on
screen and does not read its context. Each Android provider already serves
`GetContext` (the `screen.read` text, at most 4,000 characters), and its
manifest declares `context`. Once pilotd or the shell maps an Android
window to `waydroid.<pkg>`, Pilot's existing context read works as it does
for Marathon apps. That is a Pilot change and is not made here. Until
then, Pilot can call `screen.read` like any other tool when the person asks
about the screen.

## Verified

- On the host (2026-10-19): `test_keelapps` (cleaning, manifests, link
  rules, rendering, and a round trip on a private `dbus-daemon` with a fake
  shell and a fake bridge: Describe, app.show, a shortcut through
  launch → apps.running → app.launch, a refused share, screen.read, the
  confirm path of ui.tap, GetContext, the lock, losing Android, the owner
  change on a new manifest, the restart from `apps.json`, refusing an
  untrusted caller).
- In the Alpine build (`deckard` aport against this repo's HEAD): all 9
  ctest suites pass, test_keelapps included.
- DeckardBridge builds with `build-apk.sh` (javac, d8, aapt).
- On the device: pending.
