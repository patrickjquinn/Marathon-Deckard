#!/system/bin/sh
# Read-only Android-side checks. Run inside lxc-attach, not on the host.
# Usage: sh check-contracts.sh before|after
# This does not launch activities, alter package state, or test rendering.
# Functions below are dispatched through check().
# shellcheck disable=SC2329
set -eu
export PATH=/system/bin:/system/xbin
phase=${1:-}
case "$phase" in before|after) ;; *) echo 'usage: check-contracts.sh before|after' >&2; exit 2 ;; esac
failed=0
check() {
    if "$@"; then echo "PASS: $*"; else echo "FAIL: $*"; failed=1; fi
}
has_package() {
    pm path "$1" | grep -q '^package:'
}
resolves() {
    action=$1
    expected=$2
    result=$(cmd package resolve-activity --brief --user 0 -a "$action" -c android.intent.category.DEFAULT)
    echo "$action => $result"
    echo "$result" | grep -Fq "$expected"
}
launcher_absent() {
    result=$(cmd package query-activities --brief --user 0 \
        -a android.intent.action.MAIN -c android.intent.category.LAUNCHER -p com.android.settings)
    echo "Settings launcher query: $result"
    ! echo "$result" | grep -q 'com.android.settings/'
}
path_absent() {
    test ! -e "$1" && test ! -L "$1"
}
check test "$(getprop sys.boot_completed)" = 1
for package in org.marathonos.deckard.home org.marathonos.deckard.system \
    org.marathonos.deckard.ime com.android.systemui com.android.permissioncontroller \
    com.android.settings com.android.keychain com.android.certinstaller com.android.vpndialogs; do
    check has_package "$package"
done
home=$(cmd package resolve-activity --brief --user 0 -a android.intent.action.MAIN -c android.intent.category.HOME)
echo "HOME => $home"
check test "$(echo "$home" | tail -n 1)" = 'org.marathonos.deckard.home/.HomeActivity'
for action in android.settings.SETTINGS android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION \
    android.settings.MANAGE_UNKNOWN_APP_SOURCES android.settings.ACCESSIBILITY_SETTINGS \
    android.settings.NOTIFICATION_POLICY_ACCESS_SETTINGS android.settings.ACTION_NOTIFICATION_LISTENER_SETTINGS; do
    check resolves "$action" 'org.marathonos.deckard.system/.Forward'
done
check resolves android.app.action.ADD_DEVICE_ADMIN 'com.android.settings/'
check resolves android.app.action.CONFIRM_DEVICE_CREDENTIAL 'com.android.settings/'
if test "$phase" = after; then
    check launcher_absent
    for path in /system/product/app/Twelve /system/product/app/Glimpse /system/product/app/Camelot \
        /system/system_ext/priv-app/Launcher3QuickStep /system/system_ext/priv-app/Updater \
        /system/system_ext/app/LocalContactsBackup /system/system_ext/priv-app/AvatarPicker \
        /system/priv-app/DeviceDiagnostics /system/priv-app/DeviceAsWebcam \
        /system/product/overlay/ApertureFrameworksBaseOverlay.apk \
        /system/product/overlay/GlimpseFrameworksBaseOverlay.apk /system/product/overlay/Launcher3Overlay.apk; do
        check path_absent "$path"
    done
fi
echo "Contract check phase=$phase failed=$failed"
exit "$failed"
