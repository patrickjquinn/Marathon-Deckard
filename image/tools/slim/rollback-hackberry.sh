#!/bin/sh
# Device-side rollback of only the slim image and its two packaged inputs.
set -eu
test "$(id -u)" = 0
backup=${1:-$(dirname -- "$0")}
backup=$(CDPATH='' cd -- "$backup" && pwd)
work=/var/lib/deckard
share=/usr/share/deckard
override=overlay/system/etc/sysconfig/deckard-runtime-apps.xml
userctl() {
    su user -c "XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus systemctl --user $*"
}
unmounted() {
    ! grep -Eq ' /var/lib/deckard/rootfs(/[^ ]*)? ' /proc/self/mountinfo
}
test -f "$backup/system.img"
echo "deec6b882038aa690a3a71730438a94abb04346b12173d15f7973ace6debc46b  $backup/system.img" | sha256sum -c -
if systemctl is-active --quiet deckard-container.service; then
    timeout 45 busctl --system call org.marathonos.Deckard.Container /ContainerManager \
        org.marathonos.Deckard.ContainerManager Stop b true
fi
userctl stop deckard-session.service deckard-bridge.service
remaining=45
until unmounted; do
    test "$remaining" -gt 0 || { echo 'Android mounts remain; restore aborted' >&2; exit 1; }
    sleep 1
    remaining=$((remaining - 1))
done
systemctl stop deckard-container.service
unmounted
ln "$backup/system.img" "$work/images/system.img.slim-restore"
mv -f "$work/images/system.img.slim-restore" "$work/images/system.img"
install -m 0644 "$backup/whiteouts" "$share/whiteouts"
if test -f "$backup/override.xml"; then
    install -D -m 0644 "$backup/override.xml" "$share/$override"
else
    rm -f "$share/$override"
fi
systemctl start deckard-container.service
userctl start deckard-session.service deckard-bridge.service
echo 'Original image restored; complete boot, contract and visual checks before handing device back.'
