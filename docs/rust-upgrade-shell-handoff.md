# Deckard Rust upgrade — Marathon-Shell team handoff

Date: 22 October 2026
Audience: Marathon-Shell integration, runtime/image and release developers
Status: Initial Rust implementation delivered in the Marathon-Deckard working tree; full migration remains in progress

## 1. Outcome to drive

Deliver the complete Rust migration and Deckard runtime replacement requested by the project owner:

- All Deckard Python and Qt C++ implementation moves to Rust.
- Shell's remaining Deckard input/window policy moves to Rust with explicit Qt bindings.
- The upstream Python host/session control plane is replaced with Deckard-owned Rust code.
- Commands, services, application identities, paths, configuration, network/container names and Android integration identifiers become Deckard names.
- Existing Android applications, files, grants, notification state and Shell associations survive migration.
- Android artifacts are rebuilt wherever changing an internal interface requires it.
- Alpine packages, device images, installation, upgrades, diagnostics, performance and rollback qualify together.

QML and Android Java framework entry points remain appropriate implementation surfaces. Qt itself, the Android platform and graphics libraries remain external dependencies. Preserve required source attribution and licenses.

This handoff supplies concrete starting code, integration contracts, work packages and release gates. It does not declare the runtime migration complete.

## 2. Actual progress and elapsed time

Implementation benchmark recorded by the session clock:

| Checkpoint | UTC on 22 October 2026 | Elapsed from start |
|---|---|---|
| First implementation inspection | 21:55:10 | 0 |
| Core and initial image-tool test results observed | 22:01:48 | 6 min 38 sec |
| Final initial-slice validation checkpoint | 22:09:13 | 14 min 03 sec |

The measured interval includes reading relevant implementation, coding, compilation, tests and correction of CLI compatibility. Writing this handoff and exporting the document occur after that checkpoint. These are wall-clock observations from one agent session, not claimed engineering person-hours.

### Delivered code

The root workspace currently contains **three implemented crates**. There is deliberately no empty Rust daemon crate standing in for the existing daemon.

| Crate | Implemented behavior |
|---|---|
| `host/deckard-core` | Validated package/app/store/transfer identities; explicit legacy-app-ID conversion; bounded incremental JSON framing; 200/50 token bucket and flood behavior; UTF-16 boundary/edit calculations; ACL validation and binary encoding; bounded PNG decode/resize/re-encode; Unicode text/base64 validation; complete 65-colour palette |
| `host/deckard-host-tools` | Native `deckard-shared-folders` LXC hook; descriptor-relative traversal, no-follow checks, owner validation, inode/device checks, no-overwrite file migration and bounded recursion; native `deckard-open-apk` with direct argument construction and JSON escaping |
| `image/deckard-image-tools` | `palette`, `fonts` and `permission-view` commands; theme token parsing; four matching font assets; guarded exactly-one-handler smali replacement; atomic individual output writes; CLI validation |

The workspace uses edition 2024, resolver 3, Rust version floor 1.88, explicit crate licenses, a committed-format lockfile, release overflow checks, thin LTO and strict Clippy checks. The lockfile exists in the working tree and still needs inclusion in the eventual reviewed commit.

Both Python generator source files were removed. `image/tools/build-chrome.sh` now invokes the Rust palette tool. `image/tools/patch-permissioncontroller.sh` now invokes the Rust smali transformation while retaining its existing stock-APK hash verification, Android compilation, signing and verification steps. These developer build scripts now require Cargo.

### Verification actually performed

- **39 Rust tests pass**: core 22; host-tool integration 8; image-tool library 5; image-tool CLI integration 4. The bounded socket-read regression was added during the handoff review after the timed initial-slice checkpoint.
- `cargo clippy --workspace --all-targets --frozen -- -D warnings` passes.
- `cargo fmt --all -- --check`, shell syntax checks and `git diff --check` pass.
- Native release builds pass.
- The ARM64 musl release build passes; its image-tool and shared-folder-hook executables were confirmed as statically linked ARM64 ELF files.
- The musl image-tool executable generated matching font XML on this host. The musl hook's no-LXC invocation was exercised.
- All 65 default palette entries match the checked-in Android overlay.
- All four generated font outputs match the checked-in outputs byte for byte.
- A differential check against the original Python palette generator passes for six highlights: teal, orange, pink, black, white and blue. The generator comment is the intentional textual difference.
- The existing C++/packaging baseline still passes **18/18 CTest suites**.

The hook tests replace mount/chown operations with recording implementations. Real privileged LXC mounts, phone execution, complete signed APK regeneration, Alpine abuild, x86_64 builds and daemon parity have not been verified in this slice. PNG validity, limits, RGBA behavior and scaling are tested; exact resampler parity with Qt has not been established.

### Production activation status

The installed `deckardd` is still the original C++ daemon. Its wire contracts and production identities have not been switched. Marathon-Shell has not been edited during this implementation slice.

The new native host helpers are buildable and tested but **are not yet selected by the consuming APKBUILD**. The old packaged Python shared-folder hook, its Python tests and the old APK-opening shell helper remain until the package can install the replacements together. The `deckard-waydroid` Python monkey-patching wrapper also remains. Do not describe this checkpoint as Python-free or Qt-free production.

## 3. Revised estimate from this work

The prior 100–200-hour estimate preceded implementation. The deterministic core/tool work moved considerably faster: the first tested slice took approximately 14 minutes. That supports reducing the forecast, but does not measure the highest-risk daemon, compositor, upstream-runtime or Android-build work.

**Current planning forecast: approximately 50–100 additional active agent hours for the full requested outcome**, with low confidence in the upstream runtime/image portion until its source and build pipeline are audited.

| Remaining work | Provisional active-hour allowance |
|---|---:|
| Complete Rust daemon and contract-level tests | 10–20 |
| Shell Rust input/window policy and identity integration | 4–8 |
| Packaging, helper activation and state/identity migration | 4–8 |
| Rust replacement for the upstream host/session control plane | 10–20 |
| Android source/image rebuild and internal identifier migration | 12–28 |
| Combined device verification, performance and rollback | 8–16 |
| Total planning interval | 48–100 |

These are remaining-work estimates, not completion commitments or a staffing estimate for the receiving team. Calendar time also depends on image builds, device availability, integration windows and discovered regressions. Re-estimate after the first real Rust daemon/Android round trip and again after the first independently booting Deckard runtime image. Do not extrapolate line count or test count from this initial slice into a percentage complete.

## 4. Source baseline and integration constraints

- Marathon-Deckard base HEAD: `1dad237815cdfe0b52432f705345772d0b0b2664`.
- Marathon-Shell inspected HEAD: `0624081b6c18902a66683e3f9081b55d57935532`.
- Shell already has extensive working-tree changes. Capture those changes before integrating; HEAD alone does not reproduce its inspected state.
- This delivery is an uncommitted Deckard working tree containing new files. Include untracked manifests, lockfile, crates and this document in the eventual review. A plain tracked-file diff omits them.
- Shell's `scripts/qemu/lib/build-deckard-apk.sh` archives a selected checkout's committed HEAD. Pointing it at the current checkout does not automatically package these uncommitted additions.
- Shell's `packaging/packages/deckard/APKBUILD` still builds C++ with CMake and packages the existing scripts and Android prebuilts.

Build the delivered slice with:

```sh
cargo test --workspace --frozen
cargo clippy --workspace --all-targets --frozen -- -D warnings
cargo fmt --all -- --check
cargo build --workspace --release --frozen
cargo build --workspace --release --target aarch64-unknown-linux-musl --frozen
```

Exercise the generators without changing checked-in assets:

```sh
cargo run --locked -p deckard-image-tools -- palette --highlight '#7df0d4' --out /tmp/deckard-colors.xml
cargo run --locked -p deckard-image-tools -- fonts --input image/vendor_deckard/fonts --out /tmp/deckard-fonts
```

The current native hook binary is `target/release/deckard-shared-folders`; it is an LXC hook, not a command to run against a live arbitrary directory. The APK opener expects one regular APK file and invokes the existing Store Keel action through `busctl`.

## 5. Work packages and ownership

Owners below are functional roles for the receiving team to assign. Each work package must include tests and observable integration evidence.

| ID | Driver | Deliverable and acceptance evidence | Dependency |
|---|---|---|---|
| D01 | Deckard/Rust | Review the delivered crates, expand protocol/validation fixtures, preserve field limits and define the transport read/drain contract | Initial slice |
| D02 | Deckard/Rust | Implement authenticated three-socket transport with `SO_PEERCRED`, package-UID lookup, bounded writes, cancellation and generation-aware reconnects | D01 |
| D03 | Deckard/Rust | Port notifications, MPRIS, audio sync and durable notification reconciliation; demonstrate reply/tap/dismiss behavior | D02 |
| D04 | Deckard/Rust | Port IME, management, packages, Settings API and permission prompts; retain signatures, job lifecycle and caller checks | D02 |
| D05 | Deckard/Rust | Port intents, file chooser/capture, sharing, location, ACL application/media indexing and Keel providers | D02 |
| D06 | Deckard/Rust | Port systemd runtime state, boot watchdog, session follower, Doze, palette sync and PSI/cgroup reclamation | D02 |
| S01 | Shell/Rust | Move remaining input filter/targeting decisions into Rust; verify CXX-Qt 0.10 bindings and event ownership | D01; can start with fixtures |
| S02 | Shell/compositor | Move Deckard window/configure/focus/security policy into Rust and resolve same-package task selection | S01; coordinated with D04/D05 |
| S03 | Shell/apps | Update Settings, Store, share, permission, task, app-registry and Keel consumers for canonical Deckard identities | Naming contract below |
| P01 | Shell/packaging | Build the implemented Rust products offline, activate both native helpers at existing paths, retain Android assets and service declarations | Initial slice; D02–D06 for daemon cutover |
| M01 | Shell + Deckard | Implement resumable, idempotent identity/path/state migration and tested rollback | Naming contract; P01 |
| R01 | Runtime | Audit exact packaged upstream source and every active CLI, D-Bus, Binder/HIDL, networking and session dependency | Can begin immediately |
| R02 | Runtime/Rust | Replace upstream host/session Python, desktop/icon export, launch/property functions and container/network management with Rust | R01 |
| A01 | Android/image | Establish reproducible source builds matching the known working image and identify every internal naming boundary | Can begin immediately |
| A02 | Android/image | Rebuild renamed HALs/properties/configuration/bridge integrations and integrate with the Rust runtime | A01; R02 |
| V01 | Release/device | Qualify clean install, upgrade, repeated boot, apps, permissions, sharing, notifications, media, power, pressure and rollback | Integrated candidates |

Do not switch the installed daemon until D02–D06 are complete. Intermediate feature slices can run against private buses and isolated data paths, so the current working installation remains usable.

## 6. Required name migration

Use these canonical names as the coordinated target contract. Readers may recognize old names during an explicit migration window. Writers and newly generated artifacts use Deckard names after cutover.

| Current surface | Canonical target | Required integration changes |
|---|---|---|
| `waydroid` command and `waydroid app/session/prop` entry points | `deckard` command with explicit app/session/property operations | Implement real operations before changing callers; migrate desktop Exec fields and diagnostics |
| System `waydroid-container.service` | System `deckard-container.service` | Unit, presets, polkit rules, start conditions, hooks, package ownership and diagnostic callers |
| User `marathon-waydroid.service` | User `deckard-session.service` | Runtime watcher/control, graphical-session integration, request marker and restart handling |
| LXC container `waydroid` | `deckard` | Configuration generation, attach/info/stop commands, networking and lifecycle cleanup |
| `/sys/fs/cgroup/lxc.payload.waydroid` | `/sys/fs/cgroup/lxc.payload.deckard` | Shell CPU/foreground policy, daemon memory reporting, reclaimer, memory limits, diagnostics; centralize path selection |
| `/var/lib/waydroid` | `/var/lib/deckard` | Images, overlay, configuration, container paths, package state and privileged helper checks |
| `~/.local/share/waydroid/data` | `~/.local/share/deckard/data` | Session mount, socket creation, Android data discovery, reset and shared-storage mapping |
| `waydroid.<package>` desktop/window/Keel identity | `deckard.<package>` | Registry, cards, task tracking, notification hints, expected-window logic, settings routes and Keel providers |
| MPRIS `waydroid_<package>` player name | `deckard_<package>` | Service name, DesktopEntry, app attribution and media-state association |
| `waydroid0` and upstream network artifacts | Deckard-owned network names/configuration | Replace the active networking implementation and its cleanup/ownership model |
| `persist.waydroid.*`, `waydroid.*` Android properties/configuration keys | Corresponding Deckard-owned keys | Rebuild every producer and consumer together; qualify the Android image |
| Upstream-named HAL modules, binaries and interfaces | Deckard-owned equivalents | Source rebuild, module discovery, service manifests, init configuration and ABI/interface compatibility |

Already correct names remain stable: `org.marathonos.Deckard`, `/org/marathonos/Deckard`, Android `/data/deckard`, and the bridge/system/IME socket basenames. Store catalogue IDs remain `android:<package>`; they are a distinct existing namespace, not a Waydroid brand.

The delivered `AppId` type validates `deckard.<package>` and contains explicit legacy conversion helpers. **It does not change the compositor's current app IDs.** Until hwcomposer and Shell agree on the new identity, production bridge code must use its actual wire identity deliberately. Avoid accidentally introducing the new type into one consumer and splitting app identity across two names.

Temporary aliases and legacy strings in migration readers, tests and historical records must be tracked separately from live runtime dependencies. Final acceptance requires a Deckard-owned production execution path and canonical identifiers. Required upstream attribution remains in source/licenses.

## 7. State migration procedure

Implement this as an explicit migration transaction with a versioned journal:

1. Stop the old user session and container; verify both stopped before changing live paths.
2. Record source paths, package versions, current app identities and ownership in a migration journal outside the moving data tree.
3. Move data on the same filesystem where possible. For cross-filesystem migration, copy with ownership/xattrs/modes preserved, verify it, and retain the original until successful activation.
4. Update image/configuration/overlay references and unit ownership; regenerate desktop entries and icons from verified package metadata.
5. Rewrite identity-bearing Shell state through schema-aware migration: favourites, saved app/card state, notification associations, permission references and any stored app routes. Discover exact stores in the current Shell implementation before editing them.
6. Preserve existing Android app UIDs and grant state. Restart into the new identity contract and verify a fixed set of installed apps.
7. Mark the transaction complete only after readiness and acceptance checks. Retrying a partial migration must be safe.
8. Test rollback by restoring the recorded paths/configuration and previous packages without deleting application data.

Do not globally replace strings in Android databases, signed packages, binary blobs or the user's files. Internal framework/HAL interfaces are rebuilt from source. Keep every ownership assumption explicit: Android UID 1000 currently coincides with the tested host user, and package-UID socket authentication relies on the present namespace model.

## 8. Shell contracts to preserve during porting

| Integration | Required behavior |
|---|---|
| Runtime readiness | A systemd-active session can still be booting. Preserve stopped/starting/running/stopping and the launch wait for Android readiness. |
| Keyboard | Keep dedicated IME transport; use UTF-16 offsets, whole surrogate pairs, OSK key identity and correct Shell-versus-Android focus routing. |
| Configure/focus | Preserve self-sizing configure behavior until the rebuilt composer no longer hotplugs unnecessarily. Android's focused and raised task must agree with the surface Marathon displays. |
| Notification tap | Expect the activity Android actually opened; avoid launching a second MAIN activity over a notification conversation or deep link. |
| Sharing/files | Preserve file-descriptor capabilities, transfer bounds and `Handoff` document grants from the bridge UID. The system UID cannot replace that provider transparently. |
| Permissions | Android retains grant authority. Preserve one-time/foreground/precise options, cancellation without grant-flag mutation and fallback when Marathon cannot draw the prompt. |
| Secure content/Pilot | Preserve secure-window checks, foreground/unlocked requirements, password exclusion, snapshot freshness, trusted callers and confirmation semantics. |
| Notifications/media | Preserve stable notification reconciliation, late action ordering, independent MPRIS/Keel name ownership, microsecond/millisecond conversions and album-art cleanup. |
| Service names | Shell interfaces use `org.marathonos.Shell`; the migration alias `.Session` is not the canonical target. The lowercase Keel service name `org.marathonos.shell` is a separate service. |

The core `Decoder` accepts bounded chunks, returns at most one accepted message per call, and retains coalesced frames. Drain it with empty input until it returns `None` before reading more bytes, and cap every socket read to `remaining_capacity()`. This preserves large frames followed by another frame without overfilling the bounded buffer. On a decoder error, close that connection; the decoder is poisoned deliberately. Check truncated EOF with `finish()`. Implement bounded outgoing writes and peer authentication in D02; those are not supplied by a pure framing library.

## 9. Packaging changes to implement

Update `Marathon-Shell/packaging/packages/deckard/APKBUILD` as a preservation-oriented migration of the current package:

- Supply `Cargo.lock`, vendored or formally cached sources, and Cargo source configuration in the build inputs. The local successful frozen build used this machine's cache; it does not establish a clean offline abuild.
- Use locked/offline builds and tests. Install `deckard-shared-folders` and `deckard-open-apk` Rust binaries at the current `/usr/libexec/` paths when activating them.
- Keep polkit paths, system/user units and `deckard-bridge.service.json` (the user bridge unit `deckard-bridge.service`, exec `/usr/libexec/deckard-bridge`) synchronized. Its `text-input` and `android-permissions` declarations remain required.
- Preserve Android APKs, microG installation, fonts, whiteouts, property files, overlay sync, reset/evidence helpers and image-specific guards.
- Keep current runtime dependencies until their actual last caller is replaced. Removing a Python generator does not remove upstream Waydroid's Python dependency.
- Remove Qt host build/runtime dependencies after the Rust daemon is selected and package dependency inspection confirms they are unnecessary. Shell itself continues to use Qt.
- Keep host/image licensing explicit. Do not change the combined package to Apache-only while distributing GPL image content.
- Verify both advertised package architectures. The successful ARM64 musl build does not establish x86_64 image support; current image properties and F-Droid catalogue filtering contain ARM64 assumptions.
- Preserve source provenance, input image digests and APK signing/rebuild records. Packaging should prove which image-specific composer and PermissionController artifacts it installs.

The future Rust daemon should use the established zbus 5.19 API family and Shell's async-io ecosystem. Shell currently resolves zvariant 5.15; do not assume its version must equal zbus's. CXX-Qt 0.10 stays in Shell's binding layer; it is unnecessary in a standalone host daemon.

## 10. Tests required before release

Carry forward the complete 18-suite host baseline and its scenarios, then verify against real Android and Shell:

1. Framing boundaries, denied UIDs, connection replacement, floods, malformed input, stalled output and reconnect cancellation.
2. D-Bus introspection/signatures, variants, descriptors, access rules and owner changes.
3. Cold app launch, expected windows, repeated session/container starts and stalled-boot recovery.
4. OSK/hardware input, emoji/surrogates, selection replacement, Back, focus and insets.
5. Notification updates, actions, replies, both dismissal directions and restart reconciliation.
6. Media controls, position/art, quiet profiles, DND and alarms.
7. File open/save, persistent grants, sharing both directions, shared-folder writes, ACLs and Gallery indexing.
8. Runtime/special permissions, precise/approximate/one-time grants, cancellation and fallback.
9. F-Droid and local APK install/update/remove, compatibility checks, microG configuration and multiple microG tasks.
10. Pilot trust, secure windows, external clients, stale accessibility snapshots and confirmation.
11. Pressure reclamation, screen-off/Doze, Shell restart, networking/clipboard recovery and idle power.
12. Clean install, in-place name/data migration, interrupted migration and complete rollback.

Record performance against the same image/device/workload, separating daemon PSS/private memory, Android process PSS, container cgroup charges and host available memory. Measure input/notification tail latency under image/package traffic and check idle wakeups. No sub-five-MB daemon claim has been established.

## 11. Ready-to-start actions for the Shell team

1. Capture the current Shell changes and assign drivers for S01/S02, P01/M01, R01 and A01.
2. Review the three Rust crates and run the commands above. Inspect new untracked files as part of the handoff.
3. Freeze the canonical naming table as a cross-repository contract and identify schema-aware Shell state migrations.
4. Build a private-bus test harness around the existing Rust Shell consumers and the future daemon; retain the working C++ daemon for device comparison.
5. Prove the pinned CXX-Qt event/QtWayland bindings early, and independently establish the exact Android image source/build pipeline.
6. Activate the native helpers through a package candidate before the daemon cutover, testing real LXC mounts and Store dispatch on the device.
7. Re-estimate at the two checkpoints in section 3 and attach build/device evidence to each completed work package.

## 12. Key source locations

| Repository | Sources |
|---|---|
| Deckard | `Cargo.toml`, `Cargo.lock`, `host/deckard-core/`, `host/deckard-host-tools/`, `image/deckard-image-tools/` |
| Deckard | `host/deckard-bridge/src/main.rs`, `protocol/bridge.md`, `Cargo.toml` |
| Deckard | `image/tools/build-chrome.sh`, `image/tools/patch-permissioncontroller.sh`, `host/packaging/` |
| Deckard | `docs/verification-2026-10-19.md`, `docs/composer-crash.md`, `docs/microg.md` |
| Shell | `crates/marathon-app-native/src/android.rs`, `crates/marathon-store-handler/src/fdroid.rs` |
| Shell | `crates/marathon-shell-native/src/{text_input,share,permissions,portal,cgroup,ipc}.rs`, `src/tasks/` under that crate |
| Shell | `shell/src/services/remotetextinput.*`, `shell/src/wayland/remotetextinputtarget.*`, `shell/src/wayland/waylandcompositor.cpp` |
| Shell | `shell/protocols/marathon-surface-v1.xml`, Settings Android QML pages, Store backend/Android page |
| Shell | `packaging/packages/deckard/APKBUILD`, device Waydroid units/configuration/polkit files, `scripts/qemu/lib/build-deckard-apk.sh` |
