# microG: Google services for Android apps

Goal from the user: "Microg instead of a Google Play Services so it can run
apps people want. Just keep it tight and optimised."

Android apps that need Google Play services (push through FCM, the fused
location API, Google sign-in, SafetyNet) run on Deckard through microG.
Only the parts that make apps work are in, and Marathon owns location.

## What is installed

| Where | What |
|---|---|
| `/system/priv-app/GmsCore` | microG Services `com.google.android.gms` 0.3.17.252432 (versionCode 252432034), the release APK unchanged, with its arm64 native code unpacked into `lib/arm64/` (Android never extracts a system app's libraries) |
| `/system/priv-app/FakeStore` | microG Companion `com.android.vending` 84022634: licence checks, in-app billing and asset delivery answers, and the "Play Store is installed" check many apps make |
| `/system/etc/permissions/privapp-permissions-microg.xml` | The privileged permissions, each granted or denied (below) |
| `/system/etc/sysconfig/microg.xml` | microG on Doze's allowlist and the data saver's, so push keeps its connection and microG needs no foreground-service notification |
| `/system/etc/microg.xml` | microG's defaults (its `SettingsProvider` reads this file): device registration, push and SafetyNet on |
| `/system/etc/init/deckard-microg.rc`, `/system/bin/deckard-microg` | At `sys.boot_completed`: microG's runtime permissions (location, accounts, notifications), and four of its components disabled: its Wi-Fi/cell `NetworkLocationService` and its system network, fused and geocoder providers |

Not installed: GsfProxy (only for C2DM, the push system before GCM; microG
says most users don't need it), UnifiedNlp backends, the Huawei (`-hw`) and
`user` builds, `com.google.android.maps.jar` (Maps API v1, retired by Google).

The APKs are not in git: GitHub refuses files over 100 MB, and they never
change here. The `deckard` aport downloads them from microG's GitHub
release (sha512 in the APKBUILD) and `image/microg/install.sh` lays them out
in the overlay after checking `image/microg/SHA256SUMS`.

### Checks on the downloads (2026-10-19)

| File | SHA-256 (matches GitHub's asset digest) |
|---|---|
| `com.google.android.gms-252432034.apk` (108,113,833 B) | `e5e6cba078bdec812b5450599350e5c1a12c8b7438da5bf77123af170f8c1a35` |
| `com.android.vending-84022634.apk` (4,210,169 B) | `9b8616011cc8f026bcbbe094df850794abb9d6d9f23214d7a8f8c4d1033ea012` |

- OpenPGP: both `.asc` signatures are good, from "microG Project Team
  <pgp@microg.org>", key `22F7 96D6 E62E 6625 A0BC EFEA 7F97 9A66 F3E0 8422`
  (keys.openpgp.org).
- APK signatures: v1 and v2 verify (apksigner); both are signed by
  `O=NOGAPPS Project, C=DE`, certificate SHA-256
  `9bd06727e62796c0130eb6dab39b73157451582cbd138e86c468acc395d14165`, which
  is also the fingerprint of microG's F-Droid repository and the SHA-256 of
  the `MICROG_REAL_SIGNATURE` LineageOS's framework checks (below).

## Signature spoofing: already in the image

Apps check that `com.google.android.gms` carries Google's signature; microG
needs the framework to report Google's certificate for it.

- Waydroid's own spoofing patch (`0007-core-Add-support-for-MicroG`, the
  `FAKE_PACKAGE_SIGNATURE` permission) exists only in its API 29 and 30
  patch sets. `waydroid-patches/base-patches-33` (lineage-20) has none.
- LineageOS put spoofing in lineage-20.0 itself:
  [383574](https://review.lineageos.org/c/LineageOS/android_frameworks_base/+/383574)
  (2024-02) and the signingInfo fix
  [411384](https://review.lineageos.org/c/LineageOS/android_frameworks_base/+/411384)
  (2024-12). `ComputerEngine.generatePackageInfo` replaces `signatures` and
  `signingInfo` with Google's certificate when the build is debuggable
  (`ANDROID_DEBUGGABLE`, compiled in for userdebug and eng), the package is
  `com.google.android.gms` or `com.android.vending`, it is signed with
  exactly microG's release certificate, and its `fake-signature` meta-data
  is Google's certificate. No permission is involved.
- Our image is LineageOS 20 VANILLA 20260927 userdebug, built from a tree
  after both changes, so the stock framework already spoofs for microG's
  official builds and nothing else.

So no framework patch, Xposed/LSPosed or Magisk is needed. The cost is the
one the design already states: it works only because the image is
userdebug, and only with microG's own signed releases (a rebuilt or
re-signed GmsCore would not be spoofed).

## Privileged permissions

Android 13 refuses to boot when a privileged app asks for a privileged
permission that `privapp-permissions` neither grants nor denies. microG
asks for 12 (from its merged manifest, against the image's
`framework-res.apk`); Companion asks for 3.

| Granted to GmsCore | Why |
|---|---|
| `INSTALL_LOCATION_PROVIDER` | microG's location service for apps |
| `CHANGE_DEVICE_IDLE_TEMP_WHITELIST` | push: lets the app a high-priority message is for run in Doze |
| `UPDATE_APP_OPS_STATS`, `WATCH_APPOPS`, `UPDATE_DEVICE_STATS` | location on behalf of apps: attribution, their app ops, wake-lock accounting |

Denied: `LOCATION_HARDWARE`, `NETWORK_SCAN`, `MODIFY_PHONE_STATE` (microG's
Wi-Fi and cell scanning; location comes from Marathon), `MANAGE_USB` and
`START_ACTIVITIES_FROM_BACKGROUND` (FIDO over USB; no USB in the
container), `DUMP` (DroidGuard), `INTERACT_ACROSS_USERS` (one user); and
for Companion `INSTALL_PACKAGES` and `DELETE_PACKAGES` (Marathon's Store
installs) and `INTERACT_ACROSS_USERS`.

Runtime permissions, granted at every boot: GmsCore `ACCESS_FINE_LOCATION`,
`ACCESS_COARSE_LOCATION`, `ACCESS_BACKGROUND_LOCATION` (restricted: microG
added to an Android that was already set up isn't exempt, so DeckardSystem
allowlists it at start), `GET_ACCOUNTS`,
`POST_NOTIFICATIONS`; Companion `GET_ACCOUNTS`, `POST_NOTIFICATIONS`. Not
granted: contacts, phone, storage, camera, body sensors, SMS, Bluetooth.

## Location from Marathon

Android 13 here has no location of its own: no GNSS HAL, and LineageOS
ships no network provider. microG's network provider would scan Wi-Fi and
cells, which the container doesn't have, and look them up online.

Instead Android's network provider is Marathon's location:

```
app → LocationManager / microG's FusedLocationProviderClient
    → Android's "network" provider = DeckardBridge NetworkLocation
    → bridge.sock: location_request {on, interval}
    → deckardd LocationSync → GeoClue (system bus) → the shell's GeoClue agent
    ← location {lat, lon, acc, time, …} ← LocationUpdated
```

- DeckardBridge's `NetworkLocation` is a `com.android.location.provider`
  `LocationProviderBase` with the v3 network provider action and
  `serviceVersion` 3, above microG's 2. `deckard-microg` also disables
  microG's `NetworkLocationProviderService` and `NetworkLocationService`,
  so microG's location service for apps takes its locations from the
  system network provider (its `hasNetworkLocationServiceBuiltIn()` is
  then false).
- microG's system fused and geocoder providers are disabled too. Bound by
  system_server, they kept microG's main process (about 64 MB PSS) running
  all the time; Android's own fused provider runs in system_server. microG's
  geocoder has no backend here anyway (its Nominatim default is off), so
  `Geocoder.isPresent()` stays false as before.
- deckardd runs one GeoClue client only while Android has a request, with
  GeoClue's accuracy level exact and `TimeThreshold` from the request's
  interval, and stops it when the request ends or the bridge drops.
- Who may locate stays with Android's runtime permission, which Marathon's
  prompt asks for (docs/permissions.md). The shell's GeoClue agent lets
  `org.marathonos.Deckard` locate, as it does the portal. Apps with
  approximate access get Android's coarsened fixes.
- The provider reports the `network` provider. Apps that ask for the
  `gps` provider by name get nothing: Android 13 can't take a GNSS provider
  from an app (Android 14 can, `config_gnssLocationProviderPackageName`).

## Settings and the Store

- Marathon Settings > Android apps > Google services (microG): Google
  services on or off (`setApplicationEnabledSetting` on GmsCore and
  Companion), push messages (microG's `gcm_enable_mcs_service`, connecting or
  disconnecting at once), the Google accounts with microG's sign-in, and
  microG's self-check. deckardd's `GetGoogleServices`, `SetGoogleServices`,
  `OpenGoogleServices` (protocol/bridge.md), done by DeckardSystem's
  `GoogleServices` as the system uid, since microG's settings provider is
  signature-protected.
- The Store's "Needs Google Play services" warning comes from DeckardBridge's
  `device` answer, whose `gms` is now true only when microG is installed and
  enabled.

- Settings opens microG's sign-in and self-check through DeckardSystem and
  then asks the shell's `ExpectAppWindow` for `waydroid.com.google.android.gms`,
  since the window comes from Android rather than a Marathon launch.

## On the HackberryPi (2026-10-19, shell a10ec2b0, deckard 20e5381)

Installed through the package path: the `deckard` aport built with the
microG sources and unpacked onto the device, `deckard-overlay-sync`, then a
session restart.

| Check | Result |
|---|---|
| Boot | PASS: system_server accepts the privapp file (no enforcement crash) |
| Self-check | PASS: "System spoofs signature", microG Services installed with the correct signature, Companion (`com.android.vending`, microG's certificate) installed with the correct signature, location (precise, approximate, background), accounts and notifications granted. Unticked by choice: GsfProxy, storage, phone, SMS, overlay |
| Check-in and push | PASS: check-in got an Android ID; `GmsGcmMcsSvc` connected to mtalk.google.com:5228 over TLS and logged in |
| Push with a real app | PASS: Element `im.vector.app` 1.6.66 gplay (official GitHub asset, SHA-256 `9c605d17…a0675d66d`, cert `f3ff38d2…2deabf5f` = Element's `assetlinks.json`), launched only, nothing signed into. Firebase asked microG to register (sender 912726360885) and Google returned an FCM token (`…:APA91b…`). Then uninstalled; microG's `gcmstatus` database has no apps or registrations left; APK deleted on both sides |
| Location | PASS: Android's `network` provider is `org.marathonos.deckard.bridge/.NetworkLocation`; a throwaway test app (uninstalled afterwards) got `Location[network 53.2439,-6.1331 hAcc=25000]` from both the `network` and `fused` providers: GeoClue's BeaconDB fix through deckardd, with the request off again afterwards. Not tested: an app calling microG's FusedLocationProviderClient (no such app installed) |
| Settings | PASS: the Google services (microG) section; push off closes the MCS connection and on reconnects within a second; Google services off disables GmsCore and Companion and hides the rest of the section; self-check opens in front. Sign-in: Android starts microG's `LoginActivity`, but while another microG task (the self-check) has a window, Marathon shows that one instead (a second Android task of the same package; see below) |
| Store | PASS: with microG off, the handler's `android-device.json` has `"gms":false`; on, `"gms":true`, so the warning clears |
| No launcher icon | PASS: Waydroid writes `waydroid.com.google.android.gms.desktop` with `NoDisplay=true` (microG is one of its system apps), so no icon in the grid, and Marathon can still show microG's windows |

### Cost

Same method as slim.md: a session restart, 2 minutes idle, then
`measure.sh` (60 s CPU window); host cost is MemAvailable with the session
stopped (60 s settled) minus with it running.

| | Baseline (no microG) | With microG |
|---|---|---|
| Summed PSS | 611.7 MB | 724.8–745.4 MB (+113–134 MB) |
| microG processes | none | `:persistent` 62–65 MB (push; service adj 500), main 64–71 MB (`cch-empty`, adj 905) |
| Anon | 220.8 MiB | 329–346 MiB |
| Processes / packages | 56 / 54 | 58–60 / 56 |
| Boot (session start to `sys.boot_completed`) | 11.0 s | 10.9–11.6 s |
| Host cost | 311 MiB | 432–453 MiB (+121–142 MiB) |
| Idle CPU, context switches | 0.15 % of a core, 69/s | 0.15–0.16 %, 65–69/s (also over a 600 s window) |
| Wake-up alarms | — | one, microG's MCS heartbeat, every 324–331 s (about 11 an hour); check-in is a non-waking RTC alarm |

- The first install had microG's own system fused and geocoder providers
  bound, which kept the main process up as well: 734 MB. Disabling them
  (above) moved it to `cch-empty`, so it's the first thing the reclaimer
  ends when memory is short. The reclaimer takes only `IMPORTANCE_CACHED`
  processes, so `:persistent` and its push connection stay. The main
  process comes back for microG's settings provider and Firebase calls.
- Push in Doze: microG is on deviceidle's `system-excidle` and `system`
  allowlists (sysconfig), so Android's Doze (Marathon-Doze's
  `force-idle`) doesn't cut its network, and no foreground-service
  notification is posted (it would reach the Hub).
- Disk: GmsCore is 108 MB plus 42 MB of unpacked libraries, in
  `/usr/share/deckard` and again in Waydroid's overlay (about 300 MB).

## Left to do

- A second Android task of the same package: when microG already has a
  window (self-check), its sign-in task's window doesn't come to the front
  in Marathon. It isn't microG-specific; the shell's window matching or
  Waydroid's per-task windows need a look.
- An app using microG's FusedLocationProviderClient on the device.
- The main process's 64 MB while idle: it only goes under memory pressure.
  AOT-compiling GmsCore (`cmd package compile`) is still to measure.
- Overlay disk: `deckard-overlay-sync` copies 150 MB of microG; hard links
  or a bind mount would halve it.
- Play Integrity stays unsupported (userdebug, test-keys), as design.md says.
