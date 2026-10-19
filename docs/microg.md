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
| `/system/etc/init/deckard-microg.rc`, `/system/bin/deckard-microg` | At `sys.boot_completed`: microG's runtime permissions (location, accounts, notifications), and microG's launcher entry and its own network-location sources disabled |

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
| `INSTALL_LOCATION_PROVIDER` | microG's fused location provider |
| `CHANGE_DEVICE_IDLE_TEMP_WHITELIST` | push: lets the app a high-priority message is for run in Doze |
| `UPDATE_APP_OPS_STATS`, `WATCH_APPOPS`, `UPDATE_DEVICE_STATS` | location on behalf of apps: attribution, their app ops, wake-lock accounting |

Denied: `LOCATION_HARDWARE`, `NETWORK_SCAN`, `MODIFY_PHONE_STATE` (microG's
Wi-Fi and cell scanning; location comes from Marathon), `MANAGE_USB` and
`START_ACTIVITIES_FROM_BACKGROUND` (FIDO over USB; no USB in the
container), `DUMP` (DroidGuard), `INTERACT_ACROSS_USERS` (one user); and
for Companion `INSTALL_PACKAGES` and `DELETE_PACKAGES` (Marathon's Store
installs) and `INTERACT_ACROSS_USERS`.

Runtime permissions, granted at every boot: GmsCore `ACCESS_FINE_LOCATION`,
`ACCESS_COARSE_LOCATION`, `ACCESS_BACKGROUND_LOCATION`, `GET_ACCOUNTS`,
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
  so microG takes its locations from the system network provider (its
  `hasNetworkLocationServiceBuiltIn()` is then false) and its fused
  provider fuses Marathon's fixes.
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
- microG's geocoder stays off (its default without a backend): it would
  send queries to Nominatim.

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

## State on 2026-10-19 (stopped before the device slot)

Committed: deckardd's location bridge (a9a6da6) and Settings API (7935771);
Marathon-Shell's GeoClue agent change (17222ea0) and the Settings section
(eb1640ef). Host tests pass (18 ctest suites, 163 Settings QML tests).

Left uncommitted, built but **not run on a device**:

- `image/microg/` (`install.sh`, `SHA256SUMS`) and the overlay files listed
  above; Marathon-Shell `packaging/packages/deckard/APKBUILD` (microG
  sources, sha512, `unzip`, the `install.sh` call).
- DeckardBridge: `NetworkLocation.java`, `stubs/`, the manifest service and
  optional `uses-library`, routing in `BridgeService`, `Requirements` (gms =
  installed and enabled). DeckardSystem: `GoogleServices.java` and the `gms`
  ops in `Manage.java`. APKs rebuilt in `build/` only, not copied to the
  overlay.
- `protocol/bridge.md`: the `device_result` gms wording.
- Device tools: `~/.cache/deckard-agents/microg/stage.sh` (pushes files,
  `apply.sh`, `revert.sh` to `/var/tmp/deckard-microg`), `restart.sh` and
  `measure.sh` (the slim ones, lock role `microg`); downloads in `…/microg/dl`.

Next, on the HackberryPi:

1. Baseline: `restart.sh`, 2 min, `measure.sh` (compare slim.md's 612–625 MB
   PSS, 246 MiB host cost, 11–16 s boot).
2. `stage.sh`, then `restart.sh 'sh /var/tmp/deckard-microg/apply.sh'`. Check
   it boots (privapp enforcement), `dumpsys package com.google.android.gms`
   (signatures spoofed, the privileged grants), the same for Companion
   (`com.android.vending`, microG's cert), the
   `/system/etc/permissions/com.android.location.provider.xml` library, the
   network provider (`dumpsys location`: DeckardBridge, not microG), no
   `waydroid.com.google.android.gms.desktop` launcher entry, and no
   foreground-service notification in the Hub.
3. microG's self-check (Settings > Google services > Check), check-in, the
   MCS connection (`dumpsys activity service …/org.microg.gms.gcm.McsService`),
   and wake-ups over 30 min idle with Doze forced.
4. Measure again: idle PSS of `com.google.android.gms` and `:persistent`,
   boot time, host cost.
5. Location: needs the shell with 17222ea0 deployed; an app gets a fix.
6. Push with a real app (approved for one test only): Element
   `vector-gplay-arm64-v8a-release-signed.apk` v1.6.66 from
   element-hq/element-android releases, SHA-256
   `9c605d17b3f2517119ea95bb0b55f5e2d582bbe7b2b1c72934e0fe9a0675d66d`,
   signed `O=New Vector Ltd.` with certificate SHA-256
   `f3ff38d2…2deabf5f`, which matches im.vector.app in Element's
   `/.well-known/assetlinks.json`. Launch only, sign into nothing, confirm the
   FCM token and microG's registration; then uninstall, clear the
   registration and delete the file (deleted here; download again).
7. If all of this passes, commit the overlay/APK part (copy the APKs into
   `image/overlay/system/priv-app/`) and the APKBUILD, and record the numbers
   here. If it fails, run `revert.sh`.

Known costs: GmsCore is 108 MB plus 42 MB of unpacked libraries, and
deckard-overlay-sync copies the overlay, so about 300 MB of disk on the
device. That still needs measuring and trimming.
