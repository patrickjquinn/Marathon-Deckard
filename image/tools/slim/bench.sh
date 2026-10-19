#!/bin/sh
# One measurement round for docs/slim.md, on the device as root:
#   ssh root@device sh -s '<command run while Android is stopped>' < bench.sh
# Host MemAvailable with Android stopped and running, boot time, the
# container's memory and idle CPU (measure.sh's numbers), cold launch of two
# apps, and frame times while scrolling Android Settings by touch.
set -u
lock=/run/deckard-session.lock
cg=/sys/fs/cgroup/lxc.payload.waydroid
apply=${1:-true}
u() { su user -c "XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus $1"; }
ac() {
	timeout 60 lxc-attach -P /var/lib/waydroid/lxc -n waydroid --clear-env -- /system/bin/sh -c \
		"export PATH=/system/bin:/apex/com.android.runtime/bin; $1" 2>/dev/null
}
tap() { MARATHON_TOUCH_WIDTH=720 MARATHON_TOUCH_HEIGHT=720 marathon-touchctl "$@"; }

while [ -f "$lock" ]; do sleep 20; done
echo "slim bench $(date +%s)" > "$lock"
trap 'rm -f "$lock"' EXIT

u 'systemctl --user stop marathon-waydroid.service'
for i in $(seq 60); do
	[ "$(lxc-info -P /var/lib/waydroid/lxc -n waydroid -s 2>/dev/null | awk '{ print $2 }')" = STOPPED ] && break
	sleep 1
done
sh -c "$apply" || { echo "apply failed"; exit 1; }
sync
sleep 20
a0=$(awk '/^MemAvailable/ { print $2 }' /proc/meminfo)
t0=$(cut -d' ' -f1 /proc/uptime)
u 'systemctl --user start marathon-waydroid.service'
for i in $(seq 600); do
	[ "$(timeout 5 lxc-attach -P /var/lib/waydroid/lxc -n waydroid -- /system/bin/getprop sys.boot_completed 2>/dev/null)" = 1 ] && break
	sleep 0.5
done
t1=$(cut -d' ' -f1 /proc/uptime)
awk -v a="$t0" -v b="$t1" 'BEGIN { printf "boot_wall_s=%.1f\n", b - a }'
sleep 120
a1=$(awk '/^MemAvailable/ { print $2 }' /proc/meminfo)
echo "host_mem_cost_mib=$(( (a0 - a1) / 1024 ))"
awk '$1 ~ /^(anon|shmem|kernel|file)$/ { printf "stat_%s_mib=%d\n", $1, $2 / 1048576 }' $cg/memory.stat
for p in $(find $cg -name cgroup.procs -exec cat {} +); do
	awk '/^Pss:/ { print $2 }' /proc/"$p"/smaps_rollup 2>/dev/null
done | awk '{ s += $1; n++ } END { printf "pss_total_mb=%d\nprocesses=%d\n", s / 1024, n }'
cs() {
	for p in $(find $cg -name cgroup.procs -exec cat {} +); do cat /proc/"$p"/task/*/status 2>/dev/null; done |
		awk '/^(non)?voluntary_ctxt_switches/ { n += $2 } END { print n + 0 }'
}
u0=$(awk '/^usage_usec/ { print $2 }' $cg/cpu.stat); c0=$(cs); w0=$(cut -d' ' -f1 /proc/uptime)
sleep 60
u1=$(awk '/^usage_usec/ { print $2 }' $cg/cpu.stat); c1=$(cs); w1=$(cut -d' ' -f1 /proc/uptime)
awk -v u0="$u0" -v u1="$u1" -v c0="$c0" -v c1="$c1" -v a="$w0" -v b="$w1" 'BEGIN {
	printf "idle_cpu_percent=%.2f\nidle_cs_per_s=%.0f\n", (u1 - u0) / 10000 / (b - a), (c1 - c0) / (b - a) }'
echo "frozen=$(cat $cg/cgroup.freeze)"

for p in net.gsantner.markor com.nononsenseapps.feeder; do
	c=$(ac "cmd package resolve-activity --brief -a android.intent.action.MAIN -c android.intent.category.LAUNCHER $p" | tail -1)
	times=""
	for i in 1 2 3; do
		ac "am force-stop $p"
		sleep 2
		t=$(ac "am start -W -n $c" | awk '/TotalTime/ { print $2 }')
		times="$times $t"
		sleep 3
	done
	ac "am force-stop $p"
	echo "cold_launch_ms_$p=$(echo $times | tr ' ' '\n' | sort -n | sed -n 2p) ($times )"
done

u 'busctl --user call org.marathonos.Shell /org/marathonos/Shell/Navigation org.marathonos.Shell.Navigation1 LaunchApp s waydroid.com.android.settings' >/dev/null
sleep 4
ac "dumpsys gfxinfo com.android.settings reset" >/dev/null
for i in 1 2 3 4 5 6; do
	tap swipe 360 560 360 160 20; sleep 0.6
	tap swipe 360 160 360 560 20; sleep 0.6
done
ac "dumpsys gfxinfo com.android.settings" | awk '/Total frames rendered|Janky frames:|50th percentile|90th percentile|99th percentile|Number Missed Vsync/ { sub(/^ +/, ""); print "scroll: " $0 }'
ac "am force-stop com.android.settings"
