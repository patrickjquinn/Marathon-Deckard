# Android as a runtime

Goal from the user: Deckard's Android (Waydroid 1.6.3, LineageOS 20 VANILLA
20260927, userdebug, test-keys, on the HackberryPi CM5) becomes a vessel that
runs Android apps for Marathon, with as much as possible taken out **without
rebuilding the Android image**. 400 MB is still too heavy.

Status: phase 2 done on 19 October 2026. Stages S1–S6, S8 and S9 are
measured, regression-tested and shipped in `image/` (whiteouts, overlay,
`waydroid.prop`); S7 was measured and left out. Results at the end.

## Where the memory goes today

Read on the device at 23:08 (Android uptime a few minutes after another
agent's session restart; DeckardHome in front, no app window open),
`dumpsys meminfo`:

- Android's own count: **864 MB PSS** in processes plus 618 MB kernel.
  The container cgroup was charged 1,548 MiB (`memory.current`, which
  includes the image's page cache).
- M0's count, host `MemAvailable` with and without the container: 706 MiB.

| Process | PSS | RSS | Why it runs |
|---|---|---|---|
| system_server | 196 MB | 332 MB | Android |
| com.android.systemui | 103 MB | 233 MB | persistent; draws nothing that reaches Marathon |
| zygote (32-bit) | **84 MB** | 168 MB | `ro.zygote=zygote64_32`; nothing 32-bit is running, so none of its pages are shared |
| surfaceflinger | 75 MB | 113 MB | composition |
| com.android.settings | 40 MB | 155 MB | started by a boot broadcast, then cached |
| app_process64 (deckard-wm) | **35 MB** | 105 MB | its own ART runtime outside zygote, so nothing is shared |
| webview_zygote | 33 MB | 77 MB | started on the first WebView use, then stays |
| zygote64 | 30 MB | 195 MB | Android |
| com.android.phone | 19 MB | 124 MB | persistent; no modem reaches Android |
| composer HAL, allocator HAL | 19 + 16 MB | | Waydroid graphics |
| MediaProvider, permission controller, network stack | 18 + 16 + 14 MB | | needed |
| deskclock, seedvault, eleven, lineageparts, waydroidupdater, contacts, keychain, statementservice, launcher3, managedprovisioning, packageinstaller, calendar provider, shell, dynsystem ×2, externalstorage, localtransport, calyx contacts backup, protonaosp deviceconfig, onetimeinitializer, lineage settingsconfig | 4–18 MB each, **213 MB together** | 85–113 MB each | each woke for `BOOT_COMPLETED`/`LOCKED_BOOT_COMPLETED`; ActivityManager keeps empty processes for 30 min (`max_empty_time_millis`) and 32 cached (`max_cached_processes`), and with 16 GB lmkd never kills them. Launcher3 runs although Deckard disables it, because the `pm disable` happens at `sys.boot_completed`, after it started. |
| DeckardHome, DeckardIme, DeckardBridge | 12 + 8 + 5 MB | | Deckard |
| ~45 native daemons and HALs | 1–12 MB each | | see *Daemons* |

By category, `.so` mappings were 305 MB, `.apk` 131 MB, `.dex` 95 MB, `.jar`
76 MB and "Unknown" (anonymous memory the A13 parser can't name on this
6.12 kernel) 393 MB.

Host-side numbers at 23:35 with apps open (`image/tools/slim/measure.sh`):
PSS 1,448 MB summed over the cgroup's 94 processes, anon 738 MiB, kernel 133
MiB, shmem 194 MiB, file 1,176 MiB. Phase 2's baseline is re-measured with
no app open, the same way as every stage.

## What was verified (phase 1)

- **Platform key.** The image's `framework-res.apk`, SystemUI, Settings,
  Shell, TeleService, DocumentsUI and IntentResolver are signed with
  SHA-256 `C8:A2:E9:BC:…:51:19:2A:B8`, exactly AOSP's public
  `build/target/product/security/platform.x509.pem` (android-13.0.0_r83,
  fetched from android.googlesource.com). The other test keys match too:
  `shared` (ContactsProvider), `media` (MediaProviderLegacy), `testkey`
  (Jelly, WebView). The keys are kept in
  `~/.marathon-secrets/deckard/aosp-test-keys/`, outside git.
  DeckardSystem.apk, signed with it, verifies with the same digest.
- **Whiteouts work.** Waydroid mounts
  `overlay ro,lowerdir=/var/lib/waydroid/overlay:/var/lib/waydroid/rootfs,upperdir=overlay_rw/system`
  (and the same for `vendor`). On the device's kernel and filesystem (ext4 on
  `marathon_data`) a `c 0 0` node in the top lower layer hid both a file and
  a whole directory of the layer below in a test mount under `/var/tmp`.
  `overlay_rw` is empty, so nothing above the overlay brings a file back.
  Removed packages are never scanned, dexopted or started; on the next boot
  PackageManager drops their state and data.
- **Props reach Android.** `/system/bin/init` (Waydroid-patched) loads
  `/vendor/waydroid.prop`, which Waydroid writes from `waydroid.cfg`
  `[properties]`; `ro.sf.lcd_density=254` already works that way. A13 init
  lets a later file override an earlier `ro.` value. To confirm in phase 2
  for the ABI lists.
- **`ro.config.low_ram` is out.** A13's
  `ActivityTaskManager.supportsMultiWindow()` returns false on a low-RAM
  device, which ends freeform windows and with them Waydroid's one window
  per task.
- **The 32-bit zygote can go without an image change.** The image has only
  `init.zygote64_32.rc` (init imports `init.${ro.zygote}.rc`). Changing
  `ro.zygote` to `zygote64` would need an `init.zygote64.rc` in the overlay,
  and a seed that reached a device before that file would leave Android with
  no zygote at all. So `ro.zygote` stays; the overlay replaces
  `init.zygote64_32.rc` with a copy whose `zygote_secondary` is `disabled`,
  and the seed empties the 32-bit ABI list. Either half alone is harmless.
  With `ro.product.cpu.abilist32` empty, `SystemServer` skips the secondary
  zygote preload (`Build.SUPPORTED_32_BIT_ABIS` empty) and PackageManager
  installs only arm64 code. Cortex-A76 runs AArch32 at EL0, so this does
  cost something: a 32-bit-only app stops installing.
- **What PackageManager refuses to boot without** (A13
  `PackageManagerService`): exactly one installer and uninstaller
  (PackageInstaller), one permission controller, the services extension
  (`android.ext.services`), the `android.ext.shared` library and the SDK
  sandbox. These are KEEP.
- **The chooser.** The coordinator relayed that `config_chooserActivity`
  (`com.android.intentresolver/.ChooserActivity`) reroutes `ACTION_CHOOSER`.
  In LineageOS 20 (`lineage-20.0` `ActivityTaskSupervisor`,
  `ActivityRecord`) and AOSP 13.0.0_r83 the only use of
  `getSystemChooserActivity()` is `ActivityRecord.isResolverOrDelegateActivity()`;
  on the device `ACTION_CHOOSER` resolves to the framework's own
  `android/com.android.internal.app.ChooserActivity` (priority 100), and
  IntentResolver's activity answers nothing. So an RRO on
  `config_chooserActivity` would not move the share sheet. A priv-app with a
  `CHOOSER` filter above priority 100 would (CHOOSER is not one of the
  priority-capped actions SEND, SENDTO, SEND_MULTIPLE, VIEW). Marathon has no
  share sheet to forward to: the shell removed its own in c0d1f559 because
  it shared nothing. So shares stay with Android's chooser (Android
  targets only) until Marathon has one; IntentResolver.apk goes (S1).
  `config_customResolverActivity` (the "open with" picker) works as
  relayed, but Marathon has no app picker for Android intents either.

## Mechanisms

| Lever | Where | Used for |
|---|---|---|
| Whiteouts | `image/whiteouts` (one path per line), installed to `/usr/share/deckard/whiteouts`; `deckard-overlay-sync` creates the `c 0 0` nodes and removes the ones a later list drops (tested on the device in a scratch tree; it never replaces a real file in the overlay) | packages, permission and feature files, unused VNDK APEXes, apps inside flattened APEX directories |
| Overlay files | `image/overlay` | `init.zygote64_32.rc` without the secondary zygote, a trimmed `vendor/etc/permissions/handheld_core_hardware.xml`, `deckard-slim.rc`, DeckardSystem |
| Props | device `waydroid.cfg` seed (`packaging/packages/device-hackberrypi-cm5-marathon/waydroid.cfg` in Marathon-Shell) | zygote, ABI lists, heap |
| Runtime settings | `deckard-slim.rc` at `sys.boot_completed` | `device_provisioned`, browser role, `max_empty_time_millis` |
| Platform-signed app | DeckardSystem (`android.uid.system`) | intent stubs, file provider, window fit |

What shipped: `image/whiteouts` (128 paths), `image/waydroid.prop` (the
ABI lists, written by `deckard-overlay-sync` because they must match the
zygote rc), and in `image/overlay`: `init.zygote64_32.rc`,
`handheld_core_hardware.xml`, `DeckardSlimRes`, `DeckardSystem` with
`deckard-system.rc`, `deckard-slim.rc`, the shared-uid DeckardHome and a
`deckard-chrome.rc` without deckard-wm. `image/slim/` keeps the staging
copies, `whiteouts.staged` with stage tags and the unshipped S7 props.
`image/tools/slim/` has `measure.sh` and `restart.sh`. For taps in
scripts on the HackberryPi, `marathon-touchctl` needs
`MARATHON_TOUCH_HEIGHT=720` (it defaults to 1440); then screenshot
coordinates are tap coordinates.

## Package inventory and verdicts

159 packages (`pm list packages -f`). **KEEP** stays; **STUB** goes once
DeckardSystem answers its intents; **REMOVE** goes. The stage is the phase-2
step that removes it.

### KEEP

| Package | Why |
|---|---|
| `android`, `lineageos.platform` | framework resources; LineageOS services in system_server |
| `com.android.providers.settings`, `org.lineageos.lineagesettings` | settings stores |
| `com.android.packageinstaller` | PackageManager throws at boot without exactly one installer/uninstaller; installs and the install-permission prompt |
| `com.android.permissioncontroller`, `com.android.safetycenter.resources` | APEX; required; runtime permission prompts (see *Needs the image*) |
| `android.ext.services`, `android.ext.shared`, `com.android.sdksandbox`, `com.android.adservices.api` | APEX or required shared library; PackageManager or system_server needs them |
| `com.android.providers.media.module` | MediaStore, the FUSE daemon behind shared storage, the photo picker |
| `com.android.providers.downloads` | DownloadManager (its notifications reach the Hub) |
| `com.android.externalstorage` | the DocumentsProvider for shared storage; on demand only |
| `com.android.providers.contacts`, `com.android.providers.calendar`, `com.android.providers.userdictionary` | apps query them; an absent provider makes `insert` throw. Empty-cursor stubs would need the image (they are `shared`-key system providers). |
| `com.android.networkstack`, `.overlay`, `.tethering`, `com.android.connectivity.resources`, `com.android.wifi.resources`, `com.android.wifi.dialog`, `com.android.uwb.resources` | connectivity; APEX resources |
| `com.android.webview` | WebView; can't be the host's engine (ABI) |
| `com.android.keychain`, `com.android.certinstaller` | KeyChain API, user CA certificates; on demand |
| `com.android.vpndialogs` | VPN apps (WireGuard, Orbot) need its consent dialog |
| `com.android.pacprocessor`, `com.android.location.fused` | PAC proxies; the location provider |
| `com.android.inputdevices` | keyboard layouts for the HackberryPi's hardware keyboard |
| `com.android.soundpicker` | apps start `RINGTONE_PICKER` and crash without a handler |
| `com.android.shell` | the shell uid's permissions, `SYSTEM_SHELL` role |
| `com.android.settings` | per-app special access has no Marathon page (see *Follow-up: Marathon Settings pages*). Kept installed but on demand only: DeckardSystem takes the generic settings pages (priority 2 over Settings' 1) and Settings' boot-started process goes after `max_empty_time_millis`. |
| Deckard: bridge, home, IME, `overlay.framework`, `overlay.palette`, `system` (new) | |

### STUB (DeckardSystem)

| Package | Replaced by | Stage |
|---|---|---|
| `org.lineageos.jelly` (BROWSER role) | `Forward` http/https → Marathon Browser | S4 |
| `com.android.documentsui` (+ its overlay) | `Pick` OPEN_DOCUMENT, GET_CONTENT, CREATE_DOCUMENT → the FileChooser portal | S4 |
| `org.lineageos.aperture` | `Pick` IMAGE_CAPTURE, VIDEO_CAPTURE → Marathon Camera | S4 |
| `com.android.gallery3d` (SYSTEM_GALLERY role) | `Pick` PICK image/video, GET_CONTENT | S4 |
| `com.android.contacts` (SYSTEM_CONTACTS role) | `Forward` tel: → Marathon Phone (its NonPhoneActivity was the only tel: handler) | S4 |
| `com.android.htmlviewer` | nothing (licence viewer) | S4 |
| settings deep links | `Forward` → Marathon Settings pages | S4 |
| `com.android.intentresolver` | nothing: unused in A13 (see *The chooser*) | S1 |

### REMOVE

| Stage | Packages | Notes |
|---|---|---|
| S1 | 48 overlays: icon packs (24), icon shapes (7), fonts (3), navigation-bar modes (6), cutout emulation (5), LineageBlackTheme, LineageNavigationBarNoHint, TrebuchetOverlay | inert; never enabled; scanned and idmapped every boot |
| S1 | Launcher3/Trebuchet, ThemePicker, Backgrounds, LiveWallpapersPicker, BasicDreams, PhotoTable, ThemesStub, WallpaperBackup, EasterEgg | Deckard already disables them |
| S1 | PrintSpooler, BuiltInPrintService, PrintRecommendationService | printing (feature removed in S6) |
| S1 | Seedvault, LocalTransport, SharedStorageBackup, BackupRestoreConfirmation, LocalContactsBackup | backup |
| S1 | DynamicSystemInstallationService, ManagedProvisioning, Provision, OneTimeInitializer, EmergencyInfo, QuickAccessWallet, StorageManager, SettingsIntelligence, WaydroidUpdater, ModuleMetadata | setup, DSU, work profiles, wallet, Waydroid's own OTA (Marathon updates the image) |
| S1 | DeskClock, Etar, ExactCalculator, Recorder, Eleven, ApertureLensLauncher, LatinIME | Marathon has these; LatinIME is disabled already |
| S1 | NfcNci, SecureElement (persistent), CompanionDeviceManager, CameraExtensionsProxy, CaptivePortalLogin, ProxyHandler, MtpService, DownloadProviderUi, StatementService, BookmarkProvider, CtsShim ×2, NetworkStackNext (a second copy; NetworkStack is the one in use) | no NFC, SE, USB or captive portal in the container; App Links verification is moot when links go to Marathon |
| S1 | OnDevicePersonalization, HalfSheetUX, OsuLogin (apps inside flattened APEX directories) | |
| S1 | VNDK APEX v28, v29, v30 | `ro.vndk.version` is 33 (`vndk.current`) |
| S1 | `android.software.live_wallpaper`, `nfc.beam`, `sip.voip` feature files | |
| S3 | SystemUI, DeckardSystemUIRes | see below |
| S5 | TeleService (persistent `com.android.phone`), TelephonyProvider, MmsService, Telecom, BlockedNumberProvider, SimAppDialog, WAPPushManager | no `android.hardware.telephony`, no modem in Android; `TelephonyManager` and `SubscriptionManager` return defaults when the `phone` and `isub` services are absent. Telecom runs inside system_server already. |
| S6 | LineageParts, Profiles, SimpleSettingsConfig, SimpleDeviceConfig; `org.lineageos.{profiles,livedisplay,health,globalactions}` features | LineageOS extras and the system_server services those features start |

### SystemUI (S3)

In multi-window mode SystemUI's windows (status bar, navigation bar,
shade, wallpaper, toasts, volume dialog) belong to no task, so Waydroid's
hwcomposer never gives them to Marathon (chrome-and-theme.md). Deckard
already turned Monet off (a static RRO now owns the palette, which works
without SystemUI), draws notifications in the Hub via DeckardBridge's
listener, and has no Launcher3. What system_server still asks of it, from
`config.xml` and A13's `SystemServer`:

| Asked for | Without SystemUI |
|---|---|
| `config_systemUIServiceComponent` started by `startSystemUi()` | `startServiceAsUser` finds nothing and returns null; `onSystemUiStarted()` still runs |
| `config_keyguardComponent` (KeyguardService) | `KeyguardServiceDelegate` logs the failed bind and treats the keyguard as not showing; Deckard has no lock screen |
| IStatusBar (StatusBarManagerService) | every call checks for a null bar: text toasts for apps targeting R+ (drawn by SystemUI) are lost, which they already are in Marathon |
| ImageWallpaper (default wallpaper component) | WallpaperManagerService fails to bind it and runs without a wallpaper; `WallpaperManager` calls still answer (the service itself stays) |
| WMShell (freeform, PiP, splits in SystemUI's process) | no TaskOrganizer: tasks are unorganised and the window manager's own freeform code places them; deckard-wm sets bounds through `IWindowOrganizerController.applyTransaction`, which doesn't need an organizer. PiP has no shell to animate it: to test. |
| `MediaProjectionPermissionActivity`, `UsbPermissionActivity`, `SlicePermissionActivity`, `SensorUseStartedActivity`, `BiometricPrompt` UI | an app that asks for screen capture gets `ActivityNotFoundException`. Screen capture never worked through Marathon anyway. A platform-signed stub could deny (or forward) it if an app needs it. |

So the plan is to remove it outright and only write a stub
`com.android.systemui` if phase 2 shows system_server or an app needs one.

### Daemons

Native services from `init` and the vendor rc files, PSS at idle:

| Keep | |
|---|---|
| init, ueventd, logd, servicemanager ×3, apexd (one shot), vold, installd, netd (+ iptables-restore), lmkd, keystore2, keymaster, gatekeeper(d), credstore, tombstoned, prng_seeder, statsd (APEX), idmap2d (on demand) | the runtime |
| surfaceflinger 75 MB, composer HAL 19 MB, allocator HAL 16 MB, gpuservice, vendor.waydroid.task | graphics and Waydroid's windows |
| audioserver 12 MB, audio HAL 5 MB, mediaserver 11 MB, media.extractor, media.codec, media.swcodec, media.metrics, drm clearkey (lazy) | playback |
| cameraserver 10 MB, external camera provider 5 MB | apps that use Camera2/CameraX directly (QR scanners) rather than the intent |
| health, power, light, vibrator, sensors, memtrack, configstore, cas HALs (≈1 MB each) | small; Waydroid's stubs |

| Candidate to stop | Saving | How |
|---|---|---|
| zygote_secondary | 84 MB | S2 |
| deckard-wm (app_process64) | ≈30 MB | S9: its code runs in DeckardSystem (`WindowFit`, a zygote child) and the `deckard-wm` service leaves `deckard-chrome.rc` (agreed with the coordinator, who told the chrome agent) |
| DeckardHome's process | ≈10 MB | S9: DeckardHome is platform-signed with `sharedUserId android.uid.system` and `android:process="org.marathonos.deckard.system"`, so its invisible home activity lives in DeckardSystem's process. It stays its own package, so `waydroid.blacklist_apps` still names only it. The manifest waits in `image/slim/apps/DeckardHome`: it can only replace the live one together with `build-chrome.sh` signing DeckardHome with the platform key, or a Deckard-key build would fail to install and leave Android without a home. |
| adbd, adb_root | 3 MB | `auto_adb = False`, but userdebug starts adbd; would need a `persist.sys.usb.config` change, to test |
| traced, traced_probes, incidentd, storaged, mdnsd, wificond | 1–2 MB each | most are `class main/late_start` with no property switch; stopping them is an rc `stop`, to try after the big stages |

## Replacement stubs: DeckardSystem

`image/apps/DeckardSystem` (package `org.marathonos.deckard.system`,
`android:sharedUserId="android.uid.system"`, persistent, built by its
`build.sh` with the platform key, 40 KB). It talks to deckardd over a new
socket of the bridge protocol, `system.sock` (protocol/bridge.md, *System
channel*), the way DeckardIme has `ime.sock`; DeckardBridge is untouched.

| Intent | Activity | Marathon |
|---|---|---|
| VIEW http/https (BROWSABLE) | `Forward` | Browser `browser.open` |
| VIEW/DIAL `tel:`, DIAL | `Forward` | Phone, number filled in (`LaunchAppWithRoute("phone", "/call")`) |
| VIEW/SENDTO `sms:` `smsto:` `mms:` `mmsto:` | `Forward` | Messages, that conversation |
| VIEW/SENDTO `mailto:` (+ EXTRA_EMAIL/CC/SUBJECT/TEXT) | `Forward` | Mail `mail.compose` |
| VIEW `geo:` | `Forward` | Maps `map.show` |
| 24 settings actions (Wi-Fi, Bluetooth, sound, display, date, storage, battery, security, input method, accounts, device info, notifications, app details, app notification and channel settings, …) | `Forward` (priority 2) | Settings page; `appmanager/waydroid.<pkg>` and `notifications/waydroid.<pkg>` for one app |
| OPEN_DOCUMENT, GET_CONTENT, PICK image/video | `Pick` (priority 200) | the FileChooser portal; files copied into `/data/deckard/xfer/<id>/`, returned as `content://org.marathonos.deckard.system.files/document/<id>%2F<name>` with a read grant (persistable for OPEN_DOCUMENT) |
| CREATE_DOCUMENT | `Pick` | portal SaveFile; the app writes the transfer file, and every close copies it to the chosen place |
| IMAGE_CAPTURE, VIDEO_CAPTURE | `Pick` | Camera `camera.open`; the next new file in `~/Pictures/Marathon` is written to the app's `EXTRA_OUTPUT` (or returned as a thumbnail / a content URI) |

deckardd's side is `host/deckardd/src/intents.cpp` with
`tests/test_intents.cpp` (all 4 test programs pass on the host build). The
same app also carries `WindowFit`, deckard-wm's logic in-process (both may
run at once: each only fixes a task that isn't already full-display
freeform).

Limits of the stubs:

- **Camera.** Marathon Camera has no "capture and return" mode
  (`camera.open` "takes no photo itself"), so deckardd waits for the next
  file in Camera's folder. A real `camera.capture` Keel action in the shell's
  camera app would be cleaner.
- **Share.** No Marathon share sheet (see *The chooser*). Protocol room is
  left; nothing intercepts `ACTION_CHOOSER` yet.
- **Messages** routes take no body, so `sms_body` is dropped.
- **Maps** text queries open Maps without searching (no Keel action shows a
  search result).
- **Files the app wants to view** (`ACTION_VIEW` on a `content:` URI: "open
  with") have no handler once Gallery and the stock apps are gone. Forwarding
  them to Marathon would copy the file out and `xdg-open` it: not done.
- **OPEN_DOCUMENT_TREE** (folder access) has no answer: a tree needs a
  DocumentsProvider that lists a host folder through deckardd.

### Window fit and the Contacts → Clock report

The chrome agent saw this: with Jelly in front, launching Contacts left
Android's top resumed activity as Clock. deckard-wm converted *every*
standard root task on the display, hidden ones included. In LineageOS 20's
`WindowOrganizerController.applyTaskChanges`, a windowing-mode change sets
`TRANSACT_EFFECTS_LIFECYCLE`. The transaction then ends in
`ensureActivitiesVisible()` and `resumeFocusedTasksTopActivities()`. A hidden
fullscreen task (Clock behind a fullscreen launch) turned freeform becomes
visible and can be resumed while the new task is still starting.
`WindowFit` now:

- skips root tasks that aren't visible (they are fitted when they show);
- changes the mode only of fullscreen tasks, and only sets bounds on
  freeform ones (no lifecycle pass);
- when it converts the top visible task, adds `reorder(token, onTop)` so the
  launching task keeps focus.

This is the likely cause from the source, not yet reproduced: phase 2
repeats the launch (Jelly in front, then Contacts) after S9 and reads
`dumpsys activity activities` for `topResumedActivity`.

## Follow-up: Marathon Settings pages

Android Settings stays installed only for the per-app grants Marathon
Settings has no page for. Each needs a Marathon page (deckardd can grant them
through `appops`/`cmd` as Android's system uid) before Settings can go:

| Grant | Android action | Granted by |
|---|---|---|
| All files access | `MANAGE_ALL_FILES_ACCESS_PERMISSION`, `MANAGE_APP_ALL_FILES_ACCESS_PERMISSION` | appop `MANAGE_EXTERNAL_STORAGE` |
| Install unknown apps (F-Droid, browsers) | `MANAGE_UNKNOWN_APP_SOURCES` | appop `REQUEST_INSTALL_PACKAGES` |
| Display over other apps | `action.MANAGE_OVERLAY_PERMISSION` | appop `SYSTEM_ALERT_WINDOW` (its windows never reach Marathon anyway) |
| Modify system settings | `action.MANAGE_WRITE_SETTINGS` | appop `WRITE_SETTINGS` |
| Usage access | `USAGE_ACCESS_SETTINGS` | appop `GET_USAGE_STATS` |
| Notification access (listeners) | `ACTION_NOTIFICATION_LISTENER_SETTINGS`, `NOTIFICATION_LISTENER_DETAIL_SETTINGS` | `cmd notification allow_listener` |
| Do Not Disturb access | `NOTIFICATION_POLICY_ACCESS_SETTINGS` | `cmd notification allow_dnd` |
| Ignore battery optimisation | `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`, `IGNORE_BATTERY_OPTIMIZATION_SETTINGS` | `cmd deviceidle whitelist +<pkg>`; Marathon's own freezer decides anyway, so this one could simply be granted |
| Alarms & reminders | `REQUEST_SCHEDULE_EXACT_ALARM` | appop `SCHEDULE_EXACT_ALARM` |
| Picture in picture | `PICTURE_IN_PICTURE_SETTINGS` | appop `PICTURE_IN_PICTURE` |
| Accessibility services | `ACCESSIBILITY_SETTINGS` | `settings put secure enabled_accessibility_services` |
| Device admin | `DeviceAdminAdd` (`ACTION_ADD_DEVICE_ADMIN`) | `dpm set-active-admin` |
| Default apps (browser, dialer, SMS, home) | `MANAGE_DEFAULT_APPS_SETTINGS`, `RoleManager` requests | PermissionController's role dialogs (an APEX app), `cmd role` |
| Per-app language | `APP_LOCALE_SETTINGS` | `cmd locale` |
| App info: permissions, storage, force stop, uninstall | `APPLICATION_DETAILS_SETTINGS` (DeckardSystem already sends it to `appmanager/waydroid.<pkg>`) | `pm grant/revoke`, `pm clear`, `am force-stop`, `pm uninstall` |
| Per-app notifications and channels | `APP_NOTIFICATION_SETTINGS`, `CHANNEL_NOTIFICATION_SETTINGS` (sent to `notifications/waydroid.<pkg>`) | `cmd notification` / INotificationManager |

## Needs the Android image rebuilt

- Removing APEX modules (adservices, ondevicepersonalization's jar, uwb,
  btservices, wifi): system_server starts their services from APEX jars, and
  the boot classpath comes from them.
- A slimmer system_server: services started unconditionally (VoiceInteraction,
  AppHibernation, Slice, TelephonyRegistry, Wallpaper unless
  `config_enableWallpaperService` is false, which would make `WallpaperManager`
  throw in apps) can only go in `SystemServer.java`.
- Empty-cursor stubs for the Contacts, Calendar and Telephony providers
  (they are `shared`-signed system providers).
- Marathon-drawn permission prompts: PermissionController is an APEX app.
- Per-app special-access grants without Android Settings.
- An arm64-only image (`waydroid_arm64_only` on SourceForge) would drop the
  32-bit libraries from the image as well; S2 already stops them running.
  Not switched: to evaluate and report to the coordinator first.
- The 30-minute default for empty processes and the boot broadcasts that
  start them, at the source (S8 shortens the wait instead).
- lmkd's `/proc/zoneinfo` parser: LineageOS 20's lmkd fails on every
  pressure event under the 6.12 kernel (`/proc/zoneinfo parse error`,
  `Failed to get free memory!`, every 10 ms while memory is short) and never
  kills. The image runs it without pressure monitors (`image/waydroid.prop`)
  so it only sets each app's `oom_score_adj`, and deckardd's reclaimer ends
  cached apps instead (bridge.md). A rebuilt lmkd that parses this kernel's
  zoneinfo could take that job back.

## Stages for phase 2

Each: take the session lock, `restart.sh` (stops the session, applies the
stage while Android is down, starts it, times the boot), wait 2 minutes,
`measure.sh 60`, regression-test the apps, screenshot and look, roll back if
an app breaks.

| Stage | Change |
|---|---|
| S0 | baseline, nothing changed |
| S1 | inert whiteouts |
| S2 | 64-bit zygote only (`init.zygote64_32.rc` with the secondary disabled + empty 32-bit ABI list) |
| S3 | no SystemUI |
| S4 | DeckardSystem in; Jelly, DocumentsUI, Aperture, Gallery, Contacts, HTMLViewer out |
| S5 | telephony out |
| S6 | trimmed `handheld_core_hardware.xml` (no Bluetooth, telecom, backup, print, companion devices, controls, managed users, voice recognisers); LineageOS extras out |
| S7 | smaller idle heaps |
| S8 | `deckard-slim.rc` (`max_empty_time_millis` 60 s, provisioning, browser role) |
| S9 | deckard-wm's service off, `WindowFit` alone; DeckardHome in DeckardSystem's process |

Hand-applying a stage on the device mirrors what the package will do:
whiteouts are `mknod` in `/var/lib/waydroid/overlay`, files are copied there,
props go into `/var/lib/waydroid/waydroid.cfg` and `waydroid_base.prop`.
Rollback removes the same nodes, files and lines.

## Measurement

`image/tools/slim/measure.sh` (device, root): `memory.current`,
`memory.peak` and `memory.stat` (anon, file, kernel, shmem, slab, page
tables) of `lxc.payload.waydroid`; host `MemAvailable`; PSS, RSS, anonymous
PSS and swap summed from every cgroup process's `smaps_rollup`, with the top
25; process and package counts; Android's boot events relative to
`ro.boottime.init`; then, if the container isn't frozen, CPU from the
cgroup's `cpu.stat` and context switches (all threads' voluntary and
involuntary) per second over the window. `restart.sh` gives
`boot_wall_s`, session start to `sys.boot_completed=1`, and counts polls
that found the container frozen (the shell froze Android during boot on 18
October; a stage measured during a freeze is repeated).

The headline numbers are anonymous memory (`stat_anon_kib` +
`stat_shmem_kib` + `stat_kernel_kib`: what the container costs that the
kernel can't drop) and total PSS. `memory.current` includes the image's page
cache and is reported but not compared.

## Regression apps

Official f-droid.org builds (`https://f-droid.org/repo/<package>_<versionCode>.apk`,
the suggested versions on 18 October 2026), installed silently with Android
muted; no media is played.

| App | Package (version code) | Exercises |
|---|---|---|
| FOSS Browser | `de.baumann.browser` (159) | WebView, downloads through DownloadManager, `<input type=file>` → GET_CONTENT, share |
| Markor | `net.gsantner.markor` (163) | OPEN_DOCUMENT and CREATE_DOCUMENT, a WebView preview, links, share |
| Catima | `me.hackerchick.catima` (1004) | IMAGE_CAPTURE with a FileProvider EXTRA_OUTPUT, image GET_CONTENT, export through CREATE_DOCUMENT |
| Feeder | `com.nononsenseapps.feeder` (4158) | background sync notifications into the Hub, links to the browser, a large Compose UI |
| Material Files | `me.zhanghai.android.files` (40) | a large UI, the all-files-access grant through Android Settings, "open with" |
| Deckard Notify Test | `org.marathonos.deckard.notifytest` | notifications, actions, replies, progress, a silent media session |

Plus `am start` checks for `tel:`, `sms:`, `mailto:`, `geo:` and the
settings actions.

## Results

Phase 2 fills this in.

Measured with the five F-Droid apps installed (165 packages at S0), no app
open, 2 minutes after `sys.boot_completed`, a 60 s CPU window. Anon, shmem
and kernel are the container cgroup's `memory.stat` in MiB; PSS is summed
from every container process's `smaps_rollup`; boot is session start to
`sys.boot_completed=1`. "Apps": the five apps launched (`am start -W`), no
crash in the crash buffer, the intents resolved as expected.

| Stage | Anon | Shmem | Kernel | PSS total | Processes | Packages | Boot | Idle CPU (one core) | Context switches/s | Apps |
|---|---|---|---|---|---|---|---|---|---|---|
| S0 | 466 | 72 | 117 | 1,063 MB | 92 | 165 | 20.7 s (36.2 s in an earlier S0 boot) | 0.07 % | 63 | all 5 OK |
| S1 | 413 | 70 | 94 | 978 MB | 80 | 71 | 12.0 s | 0.07 % | 63 | all 5 OK |
| S2 | 371 | 73 | 90 | 874 MB | 78 | 71 | 11.6 s | 0.07 % | 63 | all 5 OK |
| S3 | 335 | 31 | 85 | 786 MB | 76 | 71 | 16.4 s | 0.06 % | 62 | all 5 OK; per-task windows, focus and Settings' all-files page work without SystemUI |
| S4 | 338 | 33 | 88 | 795 MB | 78 | 65 | 11.7 s | 0.07 % | 62 | all 5 OK; links → Marathon Browser, app details → Marathon Settings, DIAL → Marathon Phone (screenshots) |
| S5 | 321 | 33 | 80 | 764 MB | 74 | 58 | 21.2 s | 0.06 % | 62 | all 5 OK; `phone`, `isub`, `telecom` services absent |
| S6 | 317 | 33 | 79 | 757 MB | 74 | 53 | 11.5 s | 0.06 % | 62 | all 5 OK (re-measured after a data reset; the first S6 window overlapped a shell redeploy) |
| S7 | 315 | 31 | 79 | 754 MB | 74 | 53 | 11.4 s | 0.06 % | 62 | all 5 OK; −3 MiB is noise, so not shipped |
| S8 | 295 | 33 | 69 | 703 MB | 68 | 53 | 16.3 s | 0.06 % | 62 | all 5 OK; seven boot-woken processes gone |
| S9 | 265 | 33 | 65 | 671 MB | 66 | 53 | 16.2 s | 0.06 % | 62 | all 5 OK; window fit converted a [124,43][596,585] task to the display, focus followed Marathon, taps reached the right task |

After shipping S1–S9, more without a rebuild (each measured the same way):

| Change | Anon | Shmem | Kernel | PSS total | Processes | Verdict |
|---|---|---|---|---|---|---|
| shipped S9 (deckard 2968950) | 265 | 33 | 65 | 671 MB | 66 | baseline for these |
| E1 `service.sf.prime_shader_cache=0` | 230 | 29 | 65 | 633 MB | 66 | shipped: SurfaceFlinger 77 → 38 MB PSS; apps draw normally (screenshots of Markor and Feeder) |
| E2 + `max_cached_processes` 2 | 221 | 29 | 61 | 621 MB | 64 | shipped: only windowless background processes go sooner |
| E3 + adbd, adb_root, traced, traced_probes, incidentd, storaged, mdnsd, credstore stopped | 214 | 29 | 61 | 611 MB | 56 | shipped |
| E4 no `preloaded-classes` (whiteout) | 241 | 29 | 65 | 639 MB | 64 | worse, and a 77 s boot: rolled back |
| killing webview_zygote after boot | | | | | | breaks WebView: no renderer starts until Android restarts. It stays. |
| S7 smaller dalvik heaps | −3 | | | | | noise, not shipped |
| **shipped final (947e0d6)** | **223** | **29** | **63** | **612–625 MB** | **58** | all 5 apps OK, WebView renders, Android Settings opens |

**Host MemAvailable cost** (session stopped, then started and settled 2
minutes; M0's method): M0 706 MiB → shipped S9 391 MiB → final **246 MiB**.

**Before → after (S0 → S9):** anonymous memory 466 → 265 MiB (−43 %),
anon + shmem + kernel 655 → 363 MiB (−45 %), summed PSS 1,063 → 671 MB
(−37 %), 92 → 66 processes, 165 → 53 packages, boot 21–36 s → 11–16 s. Idle
CPU was already low (0.06–0.07 % of a core, 62 context switches/s) and
stayed there.

What is left at idle (S9 top PSS): system_server 186 MB, surfaceflinger
77, zygote64 49, MediaProvider 24, PermissionController 22, DeckardSystem
(with DeckardHome) 22, composer HAL 20, NetworkStack 20, acore 17,
mediaserver 17, allocator HAL 16, DeckardIme 14, android.process.media 14,
webview_zygote 13, audioserver 12, DeckardBridge 12, ext.services 11,
cameraserver 11, then the media codecs and small HALs.

Boot times vary by several seconds run to run (the composer HAL aborts
once at every session start, "Binder threadpool cannot be shrunk after
starting", and Android waits for it); only differences of 10 s or more
mean anything.

Fixed on the way:

- **S2.** `disabled` doesn't hold `zygote_secondary`: init.rc starts it by
  name on `zygote-start`. With the 32-bit list empty, app_process32 aborts
  ("Unable to determine ABI list from property ro.product.cpu.abilist32"),
  and its `onrestart restart zygote` took system_server down in a loop. The
  overlay's rc now gives it `/system/bin/true`, oneshot. The 64-bit zygote
  doesn't wait for it, because its ABI list equals `ro.product.cpu.abilist`.
  The props must never reach a device without that rc, so they moved from
  the device seed into the deckard package (`image/waydroid.prop`, written
  by `deckard-overlay-sync`).
- **S3.** With no ImageWallpaper, WindowManager held `boot_completed` for its
  30 s `BOOT TIMEOUT` waiting for a wallpaper window. `DeckardSlimRes` (a
  separate static RRO on `android`) sets `config_checkWallpaperAtBoot`
  false.
- **S8.** `max_empty_time_millis` changed nothing: OomAdjuster only trims
  empty processes beyond `CUR_TRIM_EMPTY_PROCESSES` (8, derived from the
  stock 32), and at idle no adjustment runs to apply it. `am kill-all` a
  minute after boot does it.
- **After a data reset** (Marathon Settings' Reset Android), nothing set
  `device_provisioned` again, because Provision is whited out, and Android
  Settings exited at once ("Device is not provisioned"). `deckard-slim.rc`
  sets it and `user_setup_complete` at every boot.
- **S4.** The FileChooser portal was called, but xdg-desktop-portal-gtk's
  dialog never appeared on Marathon ("Unhandled parent window type"). With
  the shell's own file chooser portal (deployed 04:1x on 19 October) the
  whole path works: OPEN_DOCUMENT from Android showed Marathon's chooser,
  the picked `~/Documents` file landed in `/data/deckard/xfer/<id>/`, and
  `content read` of DeckardSystem's document URI returned its contents.
  deckardd also closes the portal request when the app gives up.

Rolled back:

- **ModuleMetadata (S1).** Without it system_server dies in a loop at
  boot: `AppRestrictionController.onSystemReady` calls `getInstalledModules`,
  and `ModuleInfoProvider` throws "Call to getInstalledModules before
  metadata loaded" when there is no metadata package.

## What is left, and what would need the image

At idle the final container runs system_server (~186 MB PSS, 44 MiB anon),
surfaceflinger (38), zygote64 (~49, mostly the shared boot image),
MediaProvider, PermissionController (re-started by role and permission jobs
after the boot kill, then cached), DeckardSystem with DeckardHome, the
composer and allocator HALs, NetworkStack, the media daemons (mediaserver,
codecs, extractor, cameraserver: needed for playback and Camera2 apps),
audioserver, DeckardIme, DeckardBridge, ext.services, webview_zygote and the
small HALs. Going further needs the image: system_server services that
start unconditionally, APEX modules, lazy media daemons, and a smaller
boot image.

## Left for Marathon

- **Share sheet.** Android shares stay in Android's chooser until Marathon
  has a share sheet (the shell's old one was removed in c0d1f559).
- **Camera.** A "capture and return" Keel action in Marathon Camera would
  replace deckardd's watch on `~/Pictures/Marathon`.
- **Messages and Maps.** A message body in Messages' route, and a Keel
  action that shows a place search.
- **Settings.** Marathon pages for Android apps' grants (*Follow-up:
  Marathon Settings pages*), after which Android Settings can go.
- **Foreground signal.** `Navigation1.ForegroundAppChanged` for deckardd's
  focus fallback (requested; the shell's keyboard-enter fix made it
  rarely needed).

