#!/bin/sh
# Makes image/overlay/vendor/lib64/hw/hwcomposer.waydroid.so from the vendor
# image's own copy (docs/composer-crash.md): hwc_binder_thread asks for the
# 4 hwbinder threads the composer service configured instead of 1, so it can
# no longer shrink the started pool and abort. Usage: patch-hwcomposer.sh
# <stock hwcomposer.waydroid.so> <out>. Only the 2026-09-27 lineage-20 build.
set -eu
before=017cefa35bccc2d8ac7bec92dabeb65422c777229a787ed395bdb9c1b9015247
after=42bef881c0fd2024d5438942eb3a070d8b486eebce28c5f4c9835b95eabf7ce0
[ "$(sha256sum < "$1" | cut -d' ' -f1)" = "$before" ] || { echo "not the expected vendor build" >&2; exit 1; }
cp "$1" "$2.tmp"
printf '\200\000\200\122' | dd of="$2.tmp" bs=1 seek=$((0x3eee0)) conv=notrunc status=none
[ "$(sha256sum < "$2.tmp" | cut -d' ' -f1)" = "$after" ] || { rm -f "$2.tmp"; echo "patch mismatch" >&2; exit 1; }
chmod 644 "$2.tmp"
mv "$2.tmp" "$2"
