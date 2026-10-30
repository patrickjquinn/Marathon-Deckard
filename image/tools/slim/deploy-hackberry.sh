#!/bin/sh
# Device-side. Execute only after the lead releases TikTok's device ownership.
# Stage the delivery directory (dereferencing system.img) before invoking.
set -eu
test "$(id -u)" = 0
test "$#" = 1 || { echo 'usage: deploy-hackberry.sh ABSOLUTE_STAGE_DIR' >&2; exit 2; }
stage=$1
case "$stage" in /*) ;; *) echo 'Stage must be absolute' >&2; exit 2 ;; esac
work=/var/lib/deckard
share=/usr/share/deckard
override=overlay/system/etc/sysconfig/deckard-runtime-apps.xml
backup=/var/lib/marathon-deploy/deckard-slim-$(date -u +%Y%m%dT%H%M%SZ)
userctl() {
    su user -c "XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus systemctl --user $*"
}
unmounted() {
    ! grep -Eq ' /var/lib/deckard/rootfs(/[^ ]*)? ' /proc/self/mountinfo
}
cd "$stage"
test -f system.img && test ! -L system.img
sha256sum -c SHA256SUMS
sha256sum -c native-before.sha256
sha256sum -c preserve-before.sha256
echo 'deec6b882038aa690a3a71730438a94abb04346b12173d15f7973ace6debc46b  /var/lib/deckard/images/system.img' | sha256sum -c -
echo '49740d5f96b220a1cf99b21e26a46424f1a2c3014f89e0a02614fe25e729c27c  /var/lib/deckard/images/vendor.img' | sha256sum -c -
test -f "$work/images/system.img" && test ! -L "$work/images/system.img"
mkdir "$backup"
# A hardlink preserves the old image without a second large copy.
ln "$work/images/system.img" "$backup/system.img"
cp -p "$share/whiteouts" "$backup/whiteouts"
if test -f "$share/$override"; then cp -p "$share/$override" "$backup/override.xml"; fi
cp native-before.sha256 "$backup/native-before.sha256"
cp preserve-before.sha256 "$backup/preserve-before.sha256"
cp rollback-hackberry.sh "$backup/rollback.sh"
cp check-contracts.sh "$backup/check-contracts.sh"
cp /proc/self/mountinfo "$backup/mountinfo-before.txt"
date -u > "$backup/deployment-started.txt"
echo "Rollback backup: $backup"
timeout 45 busctl --system call org.marathonos.Deckard.Container /ContainerManager \
    org.marathonos.Deckard.ContainerManager Stop b true
userctl stop deckard-session.service deckard-bridge.service
remaining=45
until unmounted; do
    test "$remaining" -gt 0 || { echo 'Android mounts remain; image untouched' >&2; exit 1; }
    sleep 1
    remaining=$((remaining - 1))
done
systemctl stop deckard-container.service
unmounted
# Stage must be on /var's filesystem. ln fails safely if it is elsewhere.
ln "$stage/system.img" "$work/images/system.img.slim-new"
mv -f "$work/images/system.img.slim-new" "$work/images/system.img"
install -m 0644 whiteouts "$share/whiteouts"
install -D -m 0644 deckard-runtime-apps.xml "$share/$override"
# ExecStartPre performs overlay and font sync while Android is stopped.
systemctl start deckard-container.service
userctl start deckard-session.service deckard-bridge.service
deadline=$(( $(date +%s) + 90 ))
until test "$(timeout 8 lxc-attach -P "$work/lxc" -n deckard --clear-env -- /system/bin/getprop sys.boot_completed 2>/dev/null || true)" = 1; do
    test "$(date +%s)" -lt "$deadline" || { echo "Boot incomplete; restore from $backup" >&2; exit 1; }
    sleep 2
done
timeout 45 lxc-attach -P "$work/lxc" -n deckard --clear-env -- \
    /system/bin/sh -s -- after < check-contracts.sh > "$backup/contracts-after.txt" 2>&1
sha256sum -c "$backup/native-before.sha256" > "$backup/native-files-after.txt"
sha256sum -c "$backup/preserve-before.sha256" > "$backup/preserved-files-after.txt"
echo "Structural checks passed. Evidence: $backup"
echo 'Still required: visually open native settings and music, then TikTok/FOSS reopen and sleep QA.'
