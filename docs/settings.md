# Android apps in Marathon Settings

Android apps are set up from Marathon's Settings, never from Android's
own: an "Android apps" page, Android apps in Installed apps, and special
access. The API is in [../protocol/bridge.md](../protocol/bridge.md)
(*Settings API*).

## Pieces

| Where | What |
|---|---|
| deckardd `src/settingsapi.*` | `org.marathonos.Deckard1` on `/org/marathonos/Deckard`, an adaptor on the shared root beside `Deckard1.Packages` |
| deckardd `src/callers.*` | The caller check: sender pid, then the shell's `Navigation1.AppIdForPid` up the process tree. Everything but `GetRuntime` and `AppIcon` needs `settings`. |
| deckardd `src/runtime.*` | Session state from systemd (`marathon-waydroid.service`), the system channel and `lxc.payload.waydroid/memory.current`; start at login in `~/.local/state/deckard/settings.json` |
| deckardd `src/manage.*` | `manage`/`managed` requests to DeckardSystem, with timeouts and cleaned answers |
| DeckardSystem `Manage.java` | Does the work as the system uid; refuses Android's own packages |
| Marathon-Shell `apps/settings` | `AndroidApps` (C++ singleton over D-Bus), `AndroidPage`, `AndroidAppsPage`, `AndroidAccessListPage`, `AndroidAccessPage`, `AndroidResetDialog`, `AndroidAppDetails` in `AppInfoPage`, Android rows in `AppManagerPage` |

Routes: `android`, `androidapps`, `androidaccess`, `androidaccess/<kind>`,
and `appmanager/waydroid.<package>` (DeckardSystem's app-details forward).
The first three are Keel pages of `app.show`.

## Decisions

- deckardd now runs for the whole login (`graphical-session.target`),
  not `PartOf=marathon-waydroid.service`: Settings shows Android stopped
  and starts it. Start at login defaults to on once Waydroid has a desktop
  entry without `NoDisplay=true`, and runs once per login
  (`$XDG_RUNTIME_DIR/deckard-login`), not on every deckardd restart.
- Reset Android is a real wipe: deckardd stops the session and runs
  `pkexec /usr/libexec/deckard-reset`, which removes Android's `/data`
  (`~/.local/share/waydroid/data`, root-owned files included) but keeps
  `deckard/` (deckardd's sockets), Waydroid's images and the overlay. The
  next start sets Android up again. Android isn't started again after a
  reset.
- Default apps and per-app language are left out: Marathon answers links,
  calls, messages and home through DeckardSystem.
- Only apps the person installed are listed or changed; Android's own
  packages (FLAG_SYSTEM) are refused in DeckardSystem.

## Verified

- deckardd: `tests/test_settingsapi.cpp` on a private bus with a fake shell
  and a fake Android: access denied for non-Settings callers (nothing
  reaches Android or systemd), argument checks, request/answer matching,
  `AppChanged`, Android's failures, runtime states, start-at-login policy,
  cleaning of answers.
- Settings: `tests/tst_androidpages.qml` (all 140 Settings tests pass),
  qmllint clean, built; pages rendered offscreen with keel-silica-shot and
  looked at.
- DeckardSystem compiles and is signed with the platform key.

## Not yet verified on the device

Everything Android-side: the hidden-API calls in `Manage.java` (grant and
revoke, `setNotificationsEnabledForPackage`, domain verification, deviceidle,
`forceStopPackage`, cache and data, app ops, listeners, admins), sizes from
StorageStatsManager on Waydroid's bind-mounted `/data`, and the caller check
against the live shell; whether polkit treats deckardd (a user service,
outside the logind session) as the active session for
`org.marathonos.deckard.reset`; how long Android's first boot after a reset
takes.
