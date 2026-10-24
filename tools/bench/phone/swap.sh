#!/bin/sh
# Usage: swap.sh cxx-bridge|rust-bridge|python-host|rust-host
# Moves the phone between Deckard stacks and back. rust-* restores the files
# and state saved in $K before the benchmark.
set -eux
K=/root/bench-backup-2026-10-24
R=/root/deckard-r11-backup
A=/usr/share/dbus-1/system-services/id.waydro.Container.service
J=/usr/share/marathon-shell/services/deckardd.service.json
user() { systemctl --user -M user@ "$@"; }
case $1 in
cxx-bridge)
	install -m755 $R/deckardd /usr/bin/deckardd
	install -D -m644 $R/deckardd.service /run/systemd/user/deckardd.service
	install -m644 $R/deckardd.service.json $J
	user daemon-reload ;;
rust-bridge)
	user stop deckardd
	rm -f /usr/bin/deckardd /run/systemd/user/deckardd.service $J
	rmdir /run/systemd/user
	user daemon-reload
	user start deckard-bridge ;;
python-host)
	install -m644 $K/waydroid-id.waydro.Container.service $A
	busctl call org.freedesktop.DBus / org.freedesktop.DBus ReloadConfig ;;
rust-host)
	tar -xzf $K/state.tgz -C / usr/share/dbus-1/system-services/id.waydro.Container.service
	busctl call org.freedesktop.DBus / org.freedesktop.DBus ReloadConfig
	rm -f /home/user/.local/share/applications/waydroid.*.desktop
	tar -xpzf $K/state.tgz -C / home/user/.local/share/applications \
		var/lib/waydroid/waydroid.cfg var/lib/waydroid/waydroid.prop \
		var/lib/waydroid/waydroid_base.prop var/lib/waydroid/waydroid.log var/lib/waydroid/lxc ;;
esac
