#!/bin/sh
# Prints the Deckard state a benchmark must leave unchanged; diff two runs.
U=/home/user
echo "units:"
for u in deckard-container waydroid-container; do
	echo "  $u $(systemctl is-active $u) $(systemctl is-enabled $u 2>/dev/null)"
done
for u in deckard-bridge deckard-session deckardd marathon-waydroid; do
	echo "  user/$u $(systemctl --user -M user@ is-active $u) $(systemctl --user -M user@ is-enabled $u 2>/dev/null)"
done
echo "android: $(lxc-info -P /var/lib/waydroid/lxc -n waydroid -sH)"
echo "files:"
for f in /usr/libexec/deckard-* /usr/bin/deckard-launch /usr/bin/deckardd /usr/bin/waydroid \
	/usr/lib/systemd/system/deckard-container.service /usr/lib/systemd/system/waydroid-container.service \
	/usr/lib/systemd/user/deckard-*.service /run/systemd/user/deckardd.service \
	/usr/share/dbus-1/system-services/id.waydro.Container.service \
	/usr/share/dbus-1/system.d/id.waydro.Container.conf \
	/usr/share/marathon-shell/services/deckard*.json \
	/var/lib/waydroid/waydroid.cfg /var/lib/waydroid/waydroid_base.prop \
	/var/lib/waydroid/lxc/waydroid/config /var/lib/waydroid/lxc/waydroid/config_nodes \
	$U/.local/share/applications/waydroid.*.desktop \
	$U/.local/state/deckard/settings.json $U/.local/state/deckard/shared-acl-v1; do
	[ -e "$f" ] && echo "  $(md5sum "$f" | cut -c1-12) $(stat -c '%a %U' "$f") $f"
done
echo "state: $(ls $U/.local/state/deckard | tr '\n' ' ')"
echo "notifications.json: $(cat $U/.local/state/deckard/notifications.json) $(stat -c %a $U/.local/state/deckard/notifications.json)"
echo "art: $(ls $U/.cache/deckard/art | wc -l) files"
echo "runtime markers: $(ls /run/user/1000 | grep -e waydroid -e deckard | tr '\n' ' ')"
echo "waydroid mounts: $(mount | grep -c /var/lib/waydroid)"
