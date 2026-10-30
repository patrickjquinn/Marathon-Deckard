# Slim Android: deployed result and handoff

**DEPLOYED on 29 October 2026, approximately 21:05 UTC.** Android restarted,
reported `sys.boot_completed=1`, and passed after-contract checks with
`failed=0`. No hardware reboot was performed. This records Kepler's deployment
and QA snapshot; the coordinating Shell agent owns subsequent deployment and combined QA.

Exclusive device mutation ownership was handed back to the coordinating
Shell agent after this QA snapshot. No additional device calls were made
during source delivery. The binary release candidate remains unpublished.

## Artifact

`SLIM_BUILD` below denotes the local USB build directory; its private mount
path and device identifiers are omitted from this source report.

| Evidence | Result |
|---|---|
| Original → slim | 1,076,338,688 → 917,745,664 bytes |
| Actual compressed saving | 158,593,024 bytes / 14.7345% |
| Slim SHA-256 | `869748073197496af8b0303ec4832fee989af679506bba8df39d1f1ba4e99bc7` |
| Original SHA-256 | `deec6b882038aa690a3a71730438a94abb04346b12173d15f7973ace6debc46b` |
| Vendor, unchanged | `49740d5f96b220a1cf99b21e26a46424f1a2c3014f89e0a02614fe25e729c27c` |
| Physical exclusion | All 81 matching whiteout targets absent; includes Twelve, Glimpse, Camelot, Launcher3QuickStep, Updater and the other Android 16 additions |
| Integrity | fsck passed; metadata of 6,423 retained paths and hashes of all 5,889 retained regular files match |
| Settings | APK retained unchanged for backend contracts; only launcher alias disabled |
| Build | One worker, nice 10, idle I/O; streamed tar pipe, no extracted tree; jobs finished |

Artifact directory:
`$SLIM_BUILD/`.
The verified image is `system-slim-v2.img`; delivery inputs and manifests are
in `delivery/`. The invalid 4 KiB stub is preserved separately as
`system.failed-image-merge-4096.img` and must never be deployed.

## Completed device QA

| Contract | Post-deploy result | Evidence |
|---|---|---|
| Required runtime packages and Deckard HOME | Passed | Device `contracts-after.txt`, `failed=0` |
| Generic Settings + five special-access routes → Forward | Passed | Resolver checks; real `SETTINGS` intent visibly opened native Settings |
| Device-admin and credential confirmation → Settings backend | Passed resolver checks | Backend APK unchanged; consent flows not exercised interactively |
| Stock Android app paths and Settings launcher absent | Passed | Physical exclusions, device path checks and visually inspected app grid |
| Native Marathon Settings and Music | Passed | All 18 file hashes unchanged; both visibly rendered; Music's All songs page opened |
| Helpers, production Shell and boot config | Preserved | Pre/post hashes passed; Shell SHA-256 at handoff `2dff705e36de2caf8d0471f2a9e30a48bef0cef4d358771bf5583a2d115dd6dc` |
| Native permission flow | Passed | Temporary QA app: native Deny/Allow returned `DENIED`/`GRANTED`; Android grant state confirmed |
| Marathon IME | Passed | Manually opened software keyboard; taps produced `cat` in Android editor and callback log |
| TikTok reopen | Passed two cycles | Actual close → native Settings open/close → TikTok reopen, both visibly rendered |
| FOSS Browser | Passed render/reopen | Fixed local page rendered and scrolled; close/reopen visibly rendered browser content |
| Fully-asleep launch / sleep-resume | **Unresolved / not validated** | See limitations below |

The lead independently viewed and accepted native Settings, Music, IME and
permission screenshots. The temporary QA APK was uninstalled successfully;
no existing app's permission grants were changed. Native Music's initial
capture was a splash; acceptance used its later rendered library and song list.

## Scrolling measurements and limits

Five upward drags used 24 touch steps per approximately 400 ms gesture and
1.5-second pauses. Content was visually confirmed before accepting samples.

| Trial | App janky / rendered | CPU frame p99 | Host presentation |
|---|---:|---:|---|
| TikTok, normal | 31 / 277 (11.19%) | 97 ms | No host trace for this trial |
| TikTok, second trial | 28 / 291 (9.62%) | 93 ms | Trace setup contaminated; see below |
| FOSS, fixed local HTML | 0 / 214 (0%) | 8 ms | 147 intervals; 4 over 25 ms; maximum 50.003 ms, p95 16.670 ms |

The earlier TikTok baseline was 22/284 (7.75%). **No jank improvement is
claimed.** Feed content differs. The second TikTok trial recreated the synthetic
touch device for each drag. Its 416.683 ms host interval includes the tool's
400 ms device-creation delay inside the marker window, so it is not a validated
active-scroll stall. Raw evidence is retained with this qualification. FOSS
used the corrected persistent-touch process. Its zero app-jank count does not
erase its measured host presentation gaps. No post-slim loaded trial ran.

The fully-asleep/cold Android launch timeout remains unresolved. The first
post-restart TikTok launch rendered without Android force-wake after Android
transitioned from Dozing to Awake; that is not a fully-asleep cold-boot test.
The final requested panel sleep returned `Access denied`; Android remained
Awake. The subsequent FOSS screenshot is **not accepted as sleep/resume QA**.
Earlier force-wake workarounds must remain labelled workarounds, not fixes.

The lead and local journal snapshots each contain **three**
`PageHeader.qml:119` null-`page` TypeErrors. Source triage points to
`Shipwright/keel/silica/qml/PageHeader.qml`: its `Qt.callLater` callback reads
`pageHeader.page` without guarding a destroyed/null header. This is consistent
with page teardown; no fix was made here. Do not claim zero QML errors. The
lead's journal review found no OOM, GPU reset, kernel I/O error or launch timeout
in its sampled interval. Android init still logs container restrictions and
missing optional services.

FOSS's external test-URL intent required explicitly raising the browser and
accepting its HTTP consent dialog. Its initial remote homepage also showed
website JavaScript errors. The benchmark ran only after the fixed local page
was visibly rendered; no remote website error page was measured.

## Durable evidence and privacy

Private evidence is stored in `qa/private-20261029/`, mode 0700, with files mode
0600 and a SHA-256 snapshot manifest. The snapshot contains 69 QA files plus its manifest, totaling
6,877,160 payload bytes: this stream's QA files from `/tmp`, including screenshots, logs,
control/benchmark scripts and the explicit ownership handoff, plus the lead's
private journal. Screenshots contain network identifiers and must not be
committed or included in a public release asset.

Key private files: `deckard-slim-device-deploy.txt`,
`deckard-slim-kepler-handoff.txt`, `slim-permission-results.txt`,
`slim-ime-result.txt`, `slim-tiktok-benchmark.txt`, `slim-tiktok-trace.txt`,
`slim-foss-benchmark.txt`, `slim-foss-wake.txt` and the matching PNGs.
Device-only raw trace paths are recorded in the private handoff; they were not
fetched after ownership release. This archive does not claim to contain those
additional device-only files.

Persistent device receipt and original-image rollback:
`/var/lib/marathon-deploy/deckard-slim-20261029T210524Z/`.
It includes `contracts-after.txt`, `native-files-after.txt`,
`preserved-files-after.txt`, the old `system.img` and `rollback.sh`.
The rollback command is
`sh /var/lib/marathon-deploy/deckard-slim-20261029T210524Z/rollback.sh`;
execution belongs to the coordinated device owner.

## Unpublished release candidate and source packaging

`release/deckard-system-lineage-23.2-slim-20261029.img.zst` is prepared on USB
for lead review: **698,690,039 bytes**, compressed with
`zstd --single-thread -6 --check` at nice 10 and idle I/O priority. Decompression
verified the frame checksum, exact 917,745,664-byte length and image SHA-256
above. Asset SHA-256:
`cc1345b836938b237f3167194c2848f4a6216300bd3c2fefa6433a3b351e4d93`.
Asset SHA-512 is recorded in `release/SHA512SUMS` and
`release/release-manifest.json`, alongside the uncompressed image SHA-512.
The candidate contains only the Android system image; no private QA or user
data is included. Nothing has been uploaded.

`release/source-review/` contains a durable copy and hash manifest of the
whiteouts, sysconfig, source-build exclusion patch, repacker, build/deploy/
rollback/contract scripts and these docs. The pre-commit snapshot records base commit
`d5c317833bbe0844eb2809a8ecf873e09379cafe` and the working changes captured
for source review. The Deckard APKBUILD installs `image/overlay` and `image/whiteouts`; its
source pin must include these changes when the package is rebuilt.
Downstream packaging must pin the reviewed source version and select a
published asset URL/checksums before shipping a release. No nonexistent commit or release URL
has been pinned here. Normal Deckard host tools and helper overlays remain
required with this image.

## Deployment procedure already completed

`image/tools/slim/deploy-hackberry.sh` verified staged/current image hashes,
native app hashes and helper/Shell/boot hashes before mutation. It preserved
the original system image with a hardlink and saved old whiteouts and the
optional previous launcher override. It then stopped Android through
ContainerManager, stopped session/bridge, waited for all rootfs/submounts to
disappear, and stopped the root service before atomically switching the image.
Only the packaged whiteouts and launcher sysconfig were updated. Root service
startup ran overlay/font sync before the user session and bridge restarted.
Vendor and native app files were not modified.

The staging directory already exists. Do not repeat deployment or any device
calls without a coordinated device handoff. Source scripts remain available for lead
review and future coordinated deployments.
