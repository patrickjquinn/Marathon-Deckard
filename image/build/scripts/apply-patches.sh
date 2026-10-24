#!/bin/bash
# Apply Deckard's lineage-23.2 patches on top of upstream Waydroid's.
#   usage: apply-patches.sh <lineage tree>
# Run after `. build/envsetup.sh; apply-waydroid-patches`. Each directory in
# ../patches is a project path with / replaced by _ (MAP lists them).
# Applied commits are skipped by subject, so the script can be rerun.
set -euo pipefail
T=$(readlink -f "$1"); P=$(readlink -f "$(dirname "$0")/../patches")
declare -A MAP=(
  [bionic]=bionic [build_make]=build/make [build_soong]=build/soong
  [external_jemalloc_new]=external/jemalloc_new
  [external_pulseaudio]=external/pulseaudio
  [external_rust_android-crates-io]=external/rust/android-crates-io
  [frameworks_libs_binary_translation]=frameworks/libs/binary_translation
  [hardware_interfaces]=hardware/interfaces
  [prebuilts_build-tools]=prebuilts/build-tools
  [prebuilts_clang_host_linux-x86]=prebuilts/clang/host/linux-x86
  [prebuilts_jdk_jdk21]=prebuilts/jdk/jdk21
  [prebuilts_rust]=prebuilts/rust
  [prebuilts_tools-lineage]=prebuilts/tools-lineage
  [system_core]=system/core
  [system_sepolicy]=system/sepolicy
)
for d in "$P"/*/; do
  n=$(basename "$d"); proj=${MAP[$n]:?unknown patch directory $n}
  for p in "$d"*.patch; do
    subj=$(awk '/^Subject: /{s=$0; f=1; next} f&&/^ /{s=s $0; next} f{exit} END{print s}' "$p" |
      sed 's/^Subject: \(\[PATCH[^]]*\] \)\?//')
    if git -C "$T/$proj" log -200 --format=%s | grep -qxF "$subj"; then
      echo "== $proj: already applied: $subj"
    else
      echo "== $proj: $subj"
      git -C "$T/$proj" am -q "$p"
    fi
  done
done
