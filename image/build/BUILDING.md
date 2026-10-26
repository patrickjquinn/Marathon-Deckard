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
5. Mesa host tools: `sudo dnf install glslang llvm-devel clang-devel
   libclc-devel spirv-llvm-translator-devel spirv-tools-devel
   python3-pycparser`, then `scripts/build-ninja.sh <dir>` and
   `scripts/build-mesa-tools.sh <dir> <tree>` (rerun
   `place-arm64-prebuilts.sh` afterwards).
6. Build: `scripts/build.sh <tree> <log> nothing`, then
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

`DGX` marks patches from jetbee's lineage-24.0 set (author kept), `port`
those that needed changes for 23.2.

| Project | Patch | Origin |
|---|---|---|
| bionic | kernel header cleaner finds the arm64 libclang | DGX |
| build/make | aarch64 host: HOST_ARCH=arm64, linux-arm64 prebuilts, HOST_OUT linux_musl-arm64 | DGX, port |
| build/soong | aarch64 host: RUSTC_BOOTSTRAP, no second host arch, musl loader in sbox | DGX, port |
| build/soong | clang version checks honour LLVM_PREBUILTS_VERSION; clang-22 warning flags | new |
| external/fmtlib | format.h includes stdlib.h for malloc (clang 22 libc++) | new |
| external/jemalloc_new | LG_PAGE 16 on aarch64 hosts | new |
| external/pulseaudio | link libsndfile_linux | new |
| external/rust/android-crates-io | aarch64-paging arm64 variant for the device only | DGX |
| frameworks/libs/binary_translation | dummy riscv64-to-x86_64 intrinsics generator on arm64 hosts | new (DGX's needs 24.0 defaults) |
| hardware/interfaces | 2.7 external camera provider prefer32 | new |
| hardware/ril | reference-ril fails unknown call-selection requests | new |
| prebuilts/build-tools | bison, flex, m4, make for the arm64 host | DGX |
| prebuilts/clang/host/linux-x86 | llvm build tools from the arm64 clang | DGX, port |
| prebuilts/jdk/jdk21 | arm64 javap and friends | DGX |
| prebuilts/rust | prebuilt musl std on a native arm64 host | DGX, port (code is in prebuilts/rust/soong on 23.2) |
| prebuilts/tools-lineage | arm64 mogrify | DGX |
| system/core | Waydroid's ashmem EPERM fallback, refreshed | Waydroid, port |
| system/sepolicy | getopt() results in an int (char is unsigned on arm64) | DGX |
| vendor/extra/init | waydroid-init detects Halium by a populated /vendor_extra | new |
| vendor/lineage | build-manifest.xml runs repo with the host python3 on arm64 | new |

Not taken from the DGX set: vendor/lineage kernel host tools (Waydroid
builds no kernel), system/librustutils no_std (no such module on 23.2),
the bison wrapper (Soong 23.2 already sets BISON_PKGDATADIR), and the
x86-only cflag filter (not needed so far).

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
11. **clang 22 warnings: `-Werror,-Wunterminated-string-initialization` in
    external/musl.** Soong turns that off for clang ≥ r574158, but its
    version check read only the release flag (r563880c), not
    `LLVM_PREBUILTS_VERSION`. `ClangVersion()`/`ClangShortVersion()` now
    honour the overrides, and the flags lineage-24.0 adds for r584948 and
    r596125 are included (`patches/build_soong`).
12. **clang 22: `-Werror,-Wunnecessary-virtual-specifier` in ICU's
    `uniset.h`** (clang 21 added it to `-Wextra`; r563880c predates that, and
    lineage-24.0's newer ICU no longer trips it). Turned off with the clang-22
    set.

13. **cronet: `unsupported option '-msse3' for target 'aarch64-linux-musl'`**
    (about 300 objects). Its generated Android.bp puts x86 flags in
    `target.host.cflags`. Soong now drops x86-only flags for arm64 host
    modules (DGX patch 3).
14. **flatc: `use of undeclared identifier 'strtoll_l'`.** musl has none;
    flatc builds with `FLATBUFFERS_LOCALE_INDEPENDENT=0` on musl
    (`patches/external_flatbuffers`).
15. **ART: `mutex 'mutator_lock_' must be acquired before 'monitor_lock_'
    [-Werror,-Wthread-safety-analysis]`.** clang 22 checks lock order under
    plain `-Wthread-safety`; it stays a warning for clang ≥ r596125.
16. **cronet's own libc++: `no member named 'strtoll_l'`.** Its
    `__config_site` hard-codes `_LIBCPP_HAS_MUSL_LIBC 0`; it now follows
    `ANDROID_HOST_MUSL` (`patches/external_cronet`).
17. **Genrules that run bison directly (one-true-awk, libpcap):
    `/nonexistent/common/bison/m4sugar/m4sugar.m4: cannot open`.** The arm64
    bison relocates only from `.../linux-x86/bin`. As in the DGX set,
    `bison` becomes `bison-arm64-wrapper.sh` around `bison.real` (placed by
    `place-arm64-prebuilts.sh`), and Android.bp and Soong carry
    `bison.real` into the sandbox.

18. **Mesa: `meson.build:740: Program 'glslangValidator' not found`**, and
    behind it the build-time compilers Waydroid takes from
    prebuilts/mesa-tools (`-Dmesa-clc=system -Dprecomp-compiler=system`),
    which are x86_64 glibc binaries. Install Fedora's `glslang`;
    `build-mesa-tools.sh` builds `mesa_clc`, `asahi_clc`,
    `panfrost_compile` and `vtn_bindgen2` from the tree's own
    external/mesa3d against Fedora's LLVM 22 (`llvm-devel clang-devel
    libclc-devel spirv-llvm-translator-devel spirv-tools-devel
    python3-pycparser`), and `place-arm64-prebuilts.sh` links them into
    prebuilts/build-tools/path/linux-arm64. `build.sh` sets
    `CCACHE_DISABLE=1`, since meson picks up /usr/bin/ccache by itself.

19. **clang 22: `-Werror,-Wnullable-to-nonnull-conversion` in abseil's
    `string_view.h`** (from DnsResolver's flags). Kept as a warning for
    clang ≥ r596125.

20. **rootcanal: `use of undeclared identifier 'malloc'` in fmtlib's
    `format.h`.** fmt 11.0.2 calls malloc and free without including
    `<stdlib.h>`, and clang-r596125's libc++ no longer includes it
    transitively. `format.h` now includes it, as fmt 11.1 does
    (`patches/external_fmtlib`).

21. **reference-ril: `variable 'atCommand' is used uninitialized whenever
    switch default is taken [-Werror,-Wsometimes-uninitialized]`.** The
    default case only asserts, so with NDEBUG it sent an uninitialized
    command. It now completes the request with
    `RIL_E_REQUEST_NOT_SUPPORTED` and returns (`patches/hardware_ril`).

22. **build-manifest.xml: `ModuleNotFoundError: No module named '_ssl'`.**
    `repo` imports ssl, and the linux-arm64 `py3-cmd` in
    prebuilts/build-tools is built without `_ssl` (the linux-x86 one has
    it). The rule uses `/usr/bin/python3` on arm64 hosts
    (`patches/vendor_lineage`).

23. **At runtime: SurfaceFlinger aborts with `Failed to create a valid
    texture`** on a mainline host (HackberryPi, V3D). `waydroid-init`
    logged `Halium vendor (/vendor_extra) detected; skipping GPU
    auto-detection`: it tested whether `/vendor_extra` exists, and
    `BOARD_ROOT_EXTRA_FOLDERS` puts that directory into every system image,
    so the minigbm allocator was never started. It now tests for a
    populated directory, which only a Halium host's bind mount gives
    (`patches/vendor_extra_init`).

Soong's `LLVM_NEXT=true` (meant for testing a newer clang) is not used: it
adds `-Wno-everything` and drops the version-gated flags.
`m -k` (keep going) collects independent failures in one pass, which
matters because each Soong change costs about 25 minutes of re-analysis.

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

Do not put a swapfile on the SD card. With one there (equal priority with
the NVMe swapfile), 9 GB of soong_build ended up on it and analysis took
twice as long. `swapoff` moved it to zram in 1.5 minutes.
