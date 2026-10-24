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

## Timings
