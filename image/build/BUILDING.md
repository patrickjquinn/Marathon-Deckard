# Building Deckard's Android image on an arm64 host

LineageOS 23.2 (Android 16 QPR2, SDK 36) with Waydroid's dev/lineage-23.2
device, vendor and hardware trees, product
`lineage_waydroid_arm64_only-bp4a-userdebug`, built natively on aarch64 Linux.
AOSP supports only x86_64 hosts; this directory holds what an arm64 host
needs. Reference machine: Apple M1 (8 cores, 16 GB), Fedora Asahi Remix 44,
16K-page kernel.

The arm64-host patch set started from
[jetbee/hk-lineageos-build-on-dgx-spark](https://github.com/jetbee/hk-lineageos-build-on-dgx-spark)
(lineage-24.0) and was ported to 23.2.

## Steps

1. Source: `repo init -u https://github.com/LineageOS/android -b lineage-23.2
   --depth=1 -g default,-darwin,-mips`, add Waydroid's local manifests
   (`vendor/extra/manifest_scripts/manifests-36` of
   waydroid/android_vendor_waydroid dev/lineage-23.2), `repo sync`.
2. Waydroid's patches: `. build/envsetup.sh; apply-waydroid-patches`.
   One of 194 conflicts (system/core ashmem, see below); `apply-patches.sh`
   carries a refreshed copy.
3. Toolchains: `scripts/fetch-arm64-prebuilts.sh <dir>`, then
   `scripts/make-rust-musl-toolchain.sh <dir> <tree>` and
   `scripts/place-arm64-prebuilts.sh <tree> <dir>`.
4. Patches: `scripts/apply-patches.sh <tree>`.
5. Build: `scripts/build.sh <tree> <log> nothing`, then
   `scripts/build.sh <tree> <log> systemimage vendorimage`.

## Toolchains

| Tool | Tree asks for | Used on arm64 | Source |
|---|---|---|---|
| clang | clang-r563880c (21) | clang-r596125 (22.0.2) | AOSP prebuilts/clang/host/linux-arm64, mirror-google-llvm-r614150-release |
| Go | 1.24.1 | 1.25.9 | AOSP prebuilts/go/linux-arm64, same branch |
| Rust | 1.88.0 | 1.88.0 aarch64-unknown-linux-musl | static.rust-lang.org |
| JDK | 21 | Temurin 21.0.13+9 | Adoptium |

`SOURCES.txt` in the toolchains directory records revisions and sha256.
Google publishes no arm64 clang 21, so clang 22 builds everything;
`build.sh` selects it with `LLVM_PREBUILTS_VERSION=clang-r596125` and
`LLVM_RELEASE_VERSION=22`.

## Patches

## Failures and fixes

In the order they appeared.

1. **repo sync left prebuilts/clang/host/linux-x86 half checked out.** Its
   index listed every file as deleted, and clang-r574158, clang-stable,
   embedded-sysroots and soong/ were missing or partial. Check every
   project with `git diff-index --cached --quiet HEAD`; repair one with
   `git read-tree HEAD; git checkout-index -a; git update-index --refresh`,
   then `git checkout --` whatever `git diff-files` still lists (one
   truncated .so here).
2. **Waydroid system/core 0020 (ashmem EPERM fallback) conflicts.** 23.2
   added a memfd seals check (b/498720726) after the readlink. Refreshed in
   `patches/system_core`: on EPERM fall back to `has_memfd_support()`, then
   still check the seals.
3. **`grep` is a shell function** (ugrep honouring ignore files) in the
   login shell. `build.sh` runs `m` under `env -i`.
4. **jemalloc aborts on 16K pages.** `prebuilts/build-tools/linux-arm64/bin`
   `ninja`, `n2` and `rkati` are static with a 4K-page jemalloc:
   `<jemalloc>: Unsupported system page size`, then
   `soong bootstrap failed with: signal: aborted`. `build-ninja.sh` builds
   AOSP's ninja fork with g++ and glibc malloc; `place-arm64-prebuilts.sh`
   links it in place of the prebuilt (kept as `ninja.jemalloc4k`). n2 and
   rkati are not used by default.
5. **`libpulse` depends on undefined `libsndfile12`.** WayDroid-ATV
   rewrote libsndfile lineage-20 on 2026-09-22 and renamed the library
   `libsndfile_linux`; pulseaudio still names the old module. Patched in
   `patches/external_pulseaudio`. Not arm64-specific.
6. **jdk8/linux-arm64 missing** (`jre/lib/rt.jar` for host Java modules):
   linked to linux-x86, whose jars are all that is used.
7. **`clippy-driver` missing** from the Rust toolchain: the prebuilt module
   needs the file even with `SOONG_DISABLE_CLIPPY=true`; the toolchain now
   installs clippy.
8. **berberis: `libberberis_intrinsics_gen_inline_headers_riscv64_to_x86_64`
   depends on disabled module `gen_riscv64_to_x86_64_intrinsics`.** The
   generator is a host tool and berberis disables arm64. 23.2 has no
   `berberis_enable_arm64_host_defaults` (the DGX patch's base), so arm64
   hosts get the dummy generator darwin already uses
   (`patches/frameworks_libs_binary_translation`). `host_linux_arm64` does
   not work there: Soong's arch merge reads `linux_<arch>`, not
   `host_linux_<arch>`.
9. **Kati: `includes non-existent modules in PRODUCT_PACKAGES:
   android.hardware.camera.provider@2.7-external-service`.** The service is
   `compile_multilib: "32"` and this product is 64-bit only; it is now
   `prefer32` (`patches/hardware_interfaces`). Not arm64-host-specific.
10. **Host jemalloc is configured for 4K pages.** Soong links every musl host
    tool against libjemalloc5 (external/jemalloc_new, `LG_PAGE 12`), so each
    would abort like the ninja prebuilt. aarch64 hosts now use `LG_PAGE 16`
    (64K), which runs on 4K, 16K and 64K kernels
    (`patches/external_jemalloc_new`).

## Timings

M1, 16 GB RAM, source and out/ on a USB 2.0 SD card (17 MB/s write),
shared with other builds.

| Step | Wall time | Peak RSS |
|---|---|---|
| `lunch` (soong_ui bootstrap + product config) | 6 min 44 s | small |
| `m nothing` from a clean out/ (Soong analysis + Kati) | 22 min 29 s | soong_build 12.5 GB |

Analysis needs swap: with 15.3 GB RAM it ran on two zstd zram devices
(10 GB + 12 GB, about 3.7:1) ahead of the disk swapfiles. Set them up
transiently before `m`:

    dev=$(sudo zramctl --find --size 12G --algorithm zstd)
    sudo mkswap $dev && sudo swapon -p 100 $dev
