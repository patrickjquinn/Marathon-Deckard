#!/bin/sh
# Restarts the Deckard session under the agents' session lock and times the
# boot (docs/slim.md). Runs on the device as root:
#   ssh root@device sh -s '<command run while Android is stopped>' < restart.sh
# Prints boot_wall_s (session start to sys.boot_completed=1) and every
# container freeze seen while Android booted.
set -u
lock=/run/deckard-session.lock
cg=/sys/fs/cgroup/lxc.payload.deckard
apply=${1:-true}
user() {
	su user -c "XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus $1"
}

while [ -f "$lock" ]; do
	set -- $(cat "$lock")
	age=$(($(date +%s) - ${3:-0}))
	[ "$age" -ge 600 ] && break
	echo "waiting for $1 ($2, ${age}s old)" >&2
	sleep 20
done
echo "slim measure $(date +%s)" > "$lock"
trap 'rm -f "$lock"' EXIT

user 'systemctl --user stop deckard-session.service deckard-bridge.service'
# The container service too: its ExecStartPre applies the overlay.
systemctl stop deckard-container.service
for i in $(seq 60); do
	[ "$(lxc-info -P /var/lib/deckard/lxc -n deckard -s 2>/dev/null | awk '{ print $2 }')" = STOPPED ] && break
	sleep 1
done
sh -c "$apply" || { echo "apply failed" >&2; exit 1; }
sync

t0=$(cut -d' ' -f1 /proc/uptime)
user 'systemctl --user start deckard-session.service deckard-bridge.service'
frozen=0
for i in $(seq 600); do
	if [ -f $cg/cgroup.freeze ] && [ "$(cat $cg/cgroup.freeze)" = 1 ]; then
		frozen=$((frozen + 1))
	fi
	done=$(timeout 5 lxc-attach -P /var/lib/deckard/lxc -n deckard -- /system/bin/getprop sys.boot_completed 2>/dev/null)
	[ "$done" = 1 ] && break
	sleep 0.5
done
t1=$(cut -d' ' -f1 /proc/uptime)
awk -v a="$t0" -v b="$t1" 'BEGIN { printf "boot_wall_s=%.1f\n", b - a }'
echo "boot_frozen_polls=$frozen"
