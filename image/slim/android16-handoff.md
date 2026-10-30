# Android 16 slimming handoff

**The real image is DEPLOYED.** Android booted and post-deploy contracts passed
with `failed=0`; native Settings/Music, forwarding, permission prompts, IME and
app reopening were visually and functionally checked. See
[verified-release.md](verified-release.md) for actual measurements, three
PageHeader errors and unvalidated sleep/asleep-launch behavior. Exclusive
device ownership has been handed back to the coordinating Shell agent; this stream is source-only.
The failed 4,096-byte output is preserved separately as
`system.failed-image-merge-4096.img`; never deploy it.

## Verified artifact

`SLIM_BUILD` below denotes the local USB build directory; its private mount
path and device identifiers are omitted from this source report.

`$SLIM_BUILD/system-slim-v2.img`

- Original: 1,076,338,688 bytes, SHA-256
  `deec6b882038aa690a3a71730438a94abb04346b12173d15f7973ace6debc46b`.
- Slim: 917,745,664 bytes, SHA-256
  `869748073197496af8b0303ec4832fee989af679506bba8df39d1f1ba4e99bc7`.
- Actual compressed saving: **158,593,024 bytes (14.7345%)**.
- All 81 whiteout targets are physically absent. Metadata for 6,423 retained
  paths matches, including SELinux labels and capability xattrs; all 5,889
  retained regular-file payload SHA-256 values match the original.
- Settings backend APK remains unchanged; the launcher-only sysconfig override
  is present with verified contents, root ownership, mode 0644 and SELinux label.
- `fsck.erofs --extract` passed as a no-output extraction/decompression check.
- Both input copies match the original source image hashes. Vendor remains
  unchanged (`49740d5f96b220a1cf99b21e26a46424f1a2c3014f89e0a02614fe25e729c27c`).
- Built with one compression worker, serial verification, nice 10 and idle I/O
  priority; no extracted tree or intermediate tar was written to USB.

The build's `delivery/` directory contains matching whiteouts, sysconfig,
contract-check script, image evidence, a retained-payload manifest and hashes.
Its `system.img` is a relative symlink to the verified artifact, not another
copy. Keep this directory next to the artifact when using that link.

Read-only device baseline passed before deployment: all nine required packages,
HOME to DeckardHome, six forwarding routes (generic Settings and five
special-access routes), and the two required Settings backend contracts. Output is staged as
`delivery/contracts-before.txt`. No application was launched or package state
changed during that baseline. Subsequent deployment, boot and reopening checks
are recorded in `verified-release.md`; sleep/resume remains unvalidated.

## Package scope

`image/whiteouts` now covers Android 16's Twelve (music), Glimpse (gallery),
Camelot (file manager), Launcher3QuickStep, Updater, LocalContactsBackup,
AvatarPicker, DeviceDiagnostics and DeviceAsWebcam, plus the redundant Aperture,
Glimpse and Launcher3 resource overlays. The previous Android 13 paths remain.
The repack applies every existing whiteout that is present in the source image;
81 paths match, containing 427,758,357 logical file bytes. The actual compressed
size saving is reported separately above.

`lineage-23.2-remove-packages.patch` adds the new module exclusions to
`device/deckard/deckard/remove_packages/Android.mk` for future source builds.
Its patch application check passes against the existing checkout. It has not
been applied there and no full Android build has been run.

Android Settings is **not physically deleted**. Generic settings and the
special-access routes audited earlier resolve to DeckardSystem's Forward
activity and Marathon Settings. `ADD_DEVICE_ADMIN` and
`CONFIRM_DEVICE_CREDENTIAL` still resolve to Android Settings activities.
`image/overlay/system/etc/sysconfig/deckard-runtime-apps.xml` disables only
`com.android.settings.Settings`, the launcher alias, leaving those contracts
available. Android 16's Settings manifest and SystemConfig parser confirm the
alias and XML format. Existing explicit per-user component enablement could
override the system default; check this during deployment.

Keep DeckardHome, DeckardSystem, DeckardIme, SystemUI, PermissionController,
KeyChain, CertInstaller, VpnDialogs, providers, WebView and the APEX contents.
The repacker refuses whiteouts naming the key runtime packages. No Marathon
native application is changed by these files.

## Interrupted build state

Build directory: `$SLIM_BUILD`.

- `input/system.img`: 1,076,338,688 bytes; metadata traversal succeeds, 6,838 paths.
- `input/vendor.img`: 174,780,416 bytes; hash matches the original source.
- `system.failed-image-merge-4096.img`: invalid 4 KiB failed output; diagnostic only.
- `system.layer.tar`, `system.layer.img`, `probe/`, `build.log`: old attempt's
  diagnostics, not inputs to the corrected streaming repacker.
- No copy, Python repack, mkfs or fsck job remained at recovery inspection.

Installed erofs-utils 1.9.4 rejects compressed file payloads in image-merge
mode with `compressed files not yet supported`. The corrected repacker streams
retained files from `dump.erofs` into `mkfs.erofs --tar=f`, without a full
extracted Android tree or an intermediate tar on disk. It limits mkfs to one
worker and verifies payloads sequentially. The lead has released the streaming
build slot alongside the Q25 kernel build; run at nice 10 and idle I/O priority.

## Checks already completed without heavy USB I/O

- Python parsing, XML structure and `git diff --check` pass.
- Metadata-only plan against the real image succeeds; all new exclusions exist.
- Compressed synthetic image round-trip in RAM passes physical exclusion and
  retained payload hashes, modes, UID/GID, nanosecond timestamps, symlinks and
  hardlinks. A second fixture confirms exact SELinux labels and Linux capability
  xattrs, non-root ownership and setuid/setgid modes. `fsck.erofs --extract`
  validates both outputs without extracting to disk.

These implementation checks were followed by real deployment and device QA.
They do not establish a jank improvement or validate fully-asleep launch and
sleep/resume; see the release evidence matrix.

## Bounded resume

Historical build command, already completed. Existing outputs are protected:

```sh
nice -n 10 ionice -c 3 sh image/tools/slim/resume-build.sh \
  "$ANDROID_IMAGE_DIR" "$SLIM_BUILD" > "$SLIM_BUILD/build-v2.log" 2>&1
```

Pass the original Android output directory as `ANDROID_IMAGE_DIR` and a
separate USB build directory as `SLIM_BUILD`. The resume command compares
SHA-256 of both input copies with the original images in that directory.
Missing copies are staged and published without replacing an existing file;
a mismatch stops the build. Verified hashes are saved in
`input/verified-inputs.json`. A nonblocking lock prevents duplicate resume jobs.
No original or vendor image is modified. The builder refuses existing outputs and existing
`.partial` files; inspect interrupted outputs before choosing another name.

For a read-only plan that reads metadata without hashing entire images:

```sh
python3 -B image/tools/slim/repack.py \
  "$SLIM_BUILD/input/system.img" \
  "$SLIM_BUILD/system-slim-v2.img" --plan
```

A successful run publishes `system-slim-v2.img` only after fsck, exact remaining
path checks, metadata comparison for every retained inode, retained hardlink
checks, SHA-256 comparison of every retained regular file, and override
content/label/ownership checks. Sidecars `system-slim-v2.json` and
`system-slim-v2.payloads.json` contain image hashes, byte counts, exclusions,
manifest hashes and retained file hashes. A failed run leaves only `.partial`;
do not deploy it. Do not update package pins until verified hashes exist.
The evidence includes actual `compressed_saved_bytes` and
`compressed_saved_percent`; logical removed bytes are reported separately.

## Deployment and rollback, coordinated with the device owner

Deployment is complete and device ownership was handed back to the
coordinating Shell agent after QA. This source-only delivery makes
no further device calls. The procedure below is retained for review.

At handoff, capture current image hashes, mount state, launcher inventory and
settings intent resolutions. Preserve the old system image, current vendor
image, overlay, package artifacts and permission state in the normal rollback
location. Stop Android through Deckard's container API and confirm every
rootfs/submount is unmounted before replacing an image. Stage the new image
on the same filesystem, verify its SHA-256, and switch it atomically. Deploy
matching whiteouts and sysconfig through the normal overlay package/sync path;
apply overlay sync before starting Android. Keep vendor unchanged.

`image/tools/slim/check-contracts.sh before|after` is a read-only Android-side
check for the required packages, HOME and generic/special-access Settings
resolvers. The after phase also checks absence of the new app paths and the
Settings launcher. Run it through a bounded LXC attachment; it never launches
activities or changes package state. For example, on the device after the
owner has handed over deployment:

```sh
timeout 45 lxc-attach -P /var/lib/deckard/lxc -n deckard --clear-env -- \
  /system/bin/sh -s -- after < /run/check-contracts.sh
```

Stage the script at `/run/check-contracts.sh` first. Save before and after
output, and treat a nonzero exit as a failed contract check. This is separate
from the required visual and performance checks below.

Verify boot completion; removed files and APK packages absent; the Settings
launcher absent; DeckardHome resolves HOME; Settings' credential and device-admin
contracts still resolve; generic Settings resolves Forward; permissions, IME,
notifications and native Marathon apps work. Visually open and reopen TikTok
and FOSS Browser, including after another Android app and after sleep. Capture
frame metrics and journal/logcat/kernel errors. Do not claim removal or launch
success from source files alone.

If boot, rendering or consent contracts fail, stop/unmount Android, restore the
old system image and matching overlay/package files, sync, restart and verify
the previous state. Do not delete Android user data as a recovery shortcut.
