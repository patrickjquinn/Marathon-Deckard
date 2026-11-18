# F-Droid and Aurora Store

Request from the user: ship F-Droid and Aurora Store with Deckard, the same
way as microG (pinned signed APKs from f-droid.org as system apps that
update themselves).

## What is installed

| Where | What |
|---|---|
| `/system/app/F-Droid` | F-Droid `org.fdroid.fdroid` 1.23.2 (versionCode 1023052, f-droid.org's suggested version) |
| `/system/priv-app/F-DroidPrivilegedExtension` | F-Droid Privileged Extension `org.fdroid.fdroid.privileged` 0.2.14 (2140) |
| `/system/etc/permissions/permissions_org.fdroid.fdroid.privileged.xml` | the extension's own file: `INSTALL_PACKAGES`, `DELETE_PACKAGES` |
| `/system/app/AuroraStore` | Aurora Store `com.aurora.store` 4.8.4 (76), f-droid.org's build |

This is F-Droid's own OTA layout (`app/src/main/scripts/80-fdroid.sh` in
privileged-extension): the client as an ordinary system app, only the small
extension privileged. The `deckard` aport downloads the APKs from
`https://f-droid.org/repo/` (sha512 in the APKBUILD) and
`image/fdroid/install.sh` lays them out after checking
`image/fdroid/SHA256SUMS`, unpacking their `lib/<abi>` code beside them.

Why the extension: without it every install, update and removal from F-Droid
shows Android's confirmation and first needs "Install unknown apps", which
is Android UI Marathon otherwise never shows (bridge.md). F-Droid's README
recommends building the extension and `F-Droid.apk` into ROMs; it trusts
only the official F-Droid signature (`ClientWhitelist`), so the privilege
does not reach other apps. 0.2.14 (2026-10) installs through public
`PackageInstaller` sessions on API 24 and later. fdroiddata keeps its
current version at 0.2.7 only because the OTA zip's file name changed.

Aurora Store asks for no privileged permission, so it stays unprivileged:
installs show Android's confirmation; updates of apps it installed, and its
own, need none (`UPDATE_PACKAGES_WITHOUT_USER_ACTION`, Android 12 and later).

Both clients are system apps, so they can't be uninstalled, and they keep
their f-droid.org signatures, so F-Droid updates itself, the extension and
Aurora over the system copies (Android keeps an updated system app in
`/data/app`). Their launchers are exported to Marathon's grid like any
installed app's.

## Aurora's Play device profile

Play offers some apps only to device profiles it accepts. TikTok is one:
with the phone's own profile (Android 16, a square or tablet-shaped screen)
Aurora shows it as not supported. An Android 15 phone profile, exported
from a Jolla phone's AppSupport (an honest, uncertified AOSP container with
the same microG), is accepted; the brand, model and fingerprint do not
matter, so `image/aurora/marathon.properties` keeps that profile under
Marathon's name.

`deckard-aurora-profile` (from `deckard-aurora.rc`, at boot completion)
adds it to Aurora's preferences once: `DEVICE_SPOOF_ENABLED` and
`DEVICE_SPOOF_PROPERTIES`, the properties as a JSON string, which is how
Aurora stores a spoof. It does nothing when Aurora already has a spoof of
the person's own, and `/data/system/deckard-aurora-profile` marks it done,
so a profile changed in Aurora's settings stays changed.
`image/aurora/make-prefs.py` regenerates
`overlay/system/etc/deckard-aurora-device.xml` from the properties.

Checked on the Zinwa Q25 (2026-11-18): from cleared Aurora data, the seed,
Aurora's onboarding and an anonymous sign-in, TikTok 47.7.3 is offered and
installs. An Aurora already signed in before the seed may need to sign in
again for Play to see the new profile.

## Checks on the downloads (2026-11-02)

| File | SHA-256 | APK signer certificate SHA-256 |
|---|---|---|
| `org.fdroid.fdroid_1023052.apk` (12,426,276 B) | `985f5181d48bb6bafd54083a048b391271e0ab28385881cc41294fb01a222762` | `43238d512c1e5eb2d6569f4a3afbf5523418b82e0a3ed1552770abb9a9c9ccab` (F-Droid's, as the extension's README gives it) |
| `org.fdroid.fdroid.privileged_2140.apk` (45,943 B) | `6261445e671bea58fd8d5b7c10140845a44a01940cc2e1ad8c52e3aa4a264e94` | `43238d51…a9c9ccab` |
| `com.aurora.store_76.apk` (9,360,088 B) | `fd9c75d90d0f4a7c132b9b4a5a2cf1992a45e03b8d8ff988b7dcfbc0db2c4d11` | `5c83c7672b929955dc0a1db89a5e6ae4389e2eae7ec939956041694e5815f532` (F-Droid's key for this app) |

- OpenPGP: all three `.asc` signatures are good, from "F-Droid
  <admin@f-droid.org>", key `37D2 C987 89D8 3119 4839 4E3E 41E7 044E 1DBA 2E89`.
- The repo index: `entry.jar` is signed by `43238d51…a9c9ccab`, and
  `index-v2.json` (matching its SHA-256 in `entry.json`) lists the same file
  hashes and signers.
- APK signatures: v1, v2 and v3 verify (apksigner).

## Left to do

- On a device: Android boots with the extension (the privapp file covers
  both of its privileged permissions), F-Droid finds the extension (Settings
  > Expert mode shows it), an install from F-Droid needs no prompt, and
  Aurora's install confirmation shows as a Marathon window.
