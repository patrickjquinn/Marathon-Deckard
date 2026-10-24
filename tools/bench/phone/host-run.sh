#!/bin/sh
# Usage: host-run.sh LABEL rust|python
# From Android and the container manager stopped: starts the container
# manager, then the session (startup and boot times), samples both idle for
# 120 s, then across ten app launches, and stops both again.
set -u
label=$1 stack=$2
cd "$(dirname "$0")"
user() { systemctl --user -M user@ "$@"; }
if [ "$stack" = rust ]; then
	container=deckard-container session=deckard-session.service
	start="systemctl --user -M user@ start $session"
else
	container=waydroid-container session=bench-waydroid-session.service
	start="systemd-run --user -M user@ --unit=$session --collect -E WAYLAND_DISPLAY=marathon-wayland-0 /usr/bin/waydroid session start"
fi
marker=/run/user/1000/marathon-waydroid-requested
: > $marker
chown user:user $marker
python3 bench.py startup "$label-container" system id.waydro.Container 0 "systemctl start $container"
python3 bench.py startup "$label-session" session id.waydro.Session 0 "$start"
rm -f $marker
sleep 30
cpid=$(systemctl show -p MainPID --value $container)
spid=$(user show -p MainPID --value $session)
for unit in system.slice/$container.service user.slice/user-1000.slice/user@1000.service/app.slice/$session; do
	for p in $(cat /sys/fs/cgroup/$unit/cgroup.procs); do
		echo "$label-procs ${unit##*/} $p $(tr '\0' ' ' < /proc/$p/cmdline | cut -c1-80)"
	done
done
./sample.sh "$label-idle" 120 "$cpid" "$spid"
rm -f done
./sample.sh "$label-work" until:done "$cpid" "$spid" &
python3 bench.py launches "$label" 10
touch done
wait
if [ "$stack" = rust ]; then
	user stop $session
else
	systemd-run --user -M user@ --wait --quiet --collect /usr/bin/waydroid session stop
	while [ "$(user is-active $session)" = active ]; do sleep 0.5; done
fi
systemctl stop $container
echo "$label android=$(lxc-info -P /var/lib/waydroid/lxc -n waydroid -sH)"
