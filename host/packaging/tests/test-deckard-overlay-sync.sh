#!/bin/sh
# deckard-overlay-sync against fake share and Waydroid directories: an
# image-specific file is installed only for its image.
# Usage: test-deckard-overlay-sync.sh <path to deckard-overlay-sync>
set -eu

helper=$1
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
DECKARD_SHARE_DIR=$W/share
DECKARD_WAYDROID_DIR=$W/waydroid
export DECKARD_SHARE_DIR DECKARD_WAYDROID_DIR
hwc=vendor/lib64/hw/hwcomposer.waydroid.so
other=system/etc/init/deckard-watch.rc
mkdir -p "$W/share/overlay/vendor/lib64/hw" "$W/share/overlay/system/etc/init" "$W/waydroid"
echo patched > "$W/share/overlay/$hwc"
echo rc > "$W/share/overlay/$other"

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

cfg() {
	printf '[waydroid]\nvendor_type = MAINLINE\n%s\n[properties]\n' "$1" > "$W/waydroid/waydroid.cfg"
}

run() {
	sh "$helper" 2> "$W/err" || fail "exited $?: $(cat "$W/err")"
}

cfg "vendor_datetime = 1790486210"
run
[ "$(cat "$W/waydroid/overlay/$hwc")" = patched ] || fail "not installed for its image"
[ -f "$W/waydroid/overlay/$other" ] || fail "other files missing"

cfg "vendor_datetime = 1799999999"
run
[ ! -e "$W/waydroid/overlay/$hwc" ] || fail "kept for another image"
grep -q "hwcomposer.waydroid.so is for vendor_datetime 1790486210, not '1799999999'" "$W/err" ||
	fail "no message: $(cat "$W/err")"
[ -f "$W/waydroid/overlay/$other" ] || fail "other files removed"

cfg "vendor_datetime=1790486210"
run
[ -f "$W/waydroid/overlay/$hwc" ] || fail "not installed without spaces around ="

cfg "system_datetime = 1790486210"
run
[ ! -e "$W/waydroid/overlay/$hwc" ] || fail "installed with no vendor_datetime"

pc=system/apex/com.android.permission/priv-app/PermissionController@TQ3A.230901.001/PermissionController.apk
mkdir -p "$W/share/overlay/$(dirname "$pc")"
echo patched > "$W/share/overlay/$pc"
cfg "vendor_datetime = 1790486210
system_datetime = 1790477549"
run
[ -f "$W/waydroid/overlay/$pc" ] || fail "PermissionController not installed for its image"
cfg "vendor_datetime = 1790486210
system_datetime = 1800000000"
run
[ ! -e "$W/waydroid/overlay/$pc" ] || fail "PermissionController kept for another system image"
[ -f "$W/waydroid/overlay/$hwc" ] || fail "vendor file removed for a system image change"

rm "$W/waydroid/waydroid.cfg"
run
[ ! -e "$W/waydroid/overlay/$hwc" ] || fail "installed with no waydroid.cfg"
[ ! -e "$W/waydroid/overlay/$pc" ] || fail "PermissionController installed with no waydroid.cfg"
echo "PASS"
