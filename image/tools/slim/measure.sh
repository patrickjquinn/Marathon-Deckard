#!/bin/sh
# Measures the running Deckard container (docs/slim.md). Runs on the device
# as root: ssh root@device sh -s [cpu-seconds] < measure.sh
# Prints key=value lines, then the 25 processes with the most PSS.
set -u
cg=/sys/fs/cgroup/lxc.payload.deckard
window=${1:-60}
ac() {
	timeout 20 lxc-attach -P /var/lib/deckard/lxc -n deckard --clear-env -- /system/bin/sh -c \
		"export PATH=/system/bin:/apex/com.android.runtime/bin; $1" 2>/dev/null
}
pids() {
	find "$cg" -name cgroup.procs -exec cat {} + 2>/dev/null
}
ctxt() {
	for p in $(pids); do
		cat /proc/"$p"/task/*/status 2>/dev/null
	done | awk '/^(non)?voluntary_ctxt_switches/ { n += $2 } END { print n + 0 }'
}

[ -d "$cg" ] || { echo "state=stopped"; exit 0; }
echo "date=$(date -Iseconds)"
echo "frozen=$(cat $cg/cgroup.freeze)"
echo "memory_current_kib=$(($(cat $cg/memory.current) / 1024))"
echo "memory_peak_kib=$(($(cat $cg/memory.peak 2>/dev/null || echo 0) / 1024))"
awk '$1 ~ /^(anon|file|kernel|shmem|file_mapped|slab|pagetables|kernel_stack)$/ { printf "stat_%s_kib=%d\n", $1, $2 / 1024 }' $cg/memory.stat
awk '/^MemAvailable/ { print "host_mem_available_kib=" $2 }' /proc/meminfo
echo "processes=$(pids | wc -l)"

pids | while read -r p; do
	name=$(tr '\0' ' ' < /proc/"$p"/cmdline 2>/dev/null | cut -d' ' -f1)
	[ -n "$name" ] || name=$(cat /proc/"$p"/comm 2>/dev/null)
	awk -v p="$p" -v n="$name" '/^Pss:/ { pss = $2 } /^Rss:/ { rss = $2 } /^Pss_Anon:/ { anon = $2 } /^Swap:/ { swap = $2 }
		END { if (rss) printf "%d %d %d %d %s %s\n", pss, rss, anon, swap, p, n }' /proc/"$p"/smaps_rollup 2>/dev/null
done | sort -rn > /tmp/deckard-slim-pss.$$
awk '{ pss += $1; rss += $2; anon += $3; swap += $4 } END { printf "pss_total_kib=%d\nrss_total_kib=%d\npss_anon_total_kib=%d\nswap_total_kib=%d\n", pss, rss, anon, swap }' /tmp/deckard-slim-pss.$$

if [ "$(cat $cg/cgroup.freeze)" = 0 ]; then
	echo "packages=$(ac 'pm list packages' | wc -l)"
	echo "boot_completed=$(ac 'getprop sys.boot_completed')"
	init=$(ac 'getprop ro.boottime.init')
	ac 'logcat -b events -d' | awk -v init="$init" '/ (boot_progress_[a-z_]+|sf_stop_bootanim): / {
		k = $6; sub(":", "", k); last[k] = $7 } END { for (k in last) if (init > 0) printf "boot_%s_ms=%d\n", k, last[k] - init / 1000000 }' | sort
	u0=$(awk '/^usage_usec/ { print $2 }' $cg/cpu.stat)
	c0=$(ctxt)
	t0=$(cut -d' ' -f1 /proc/uptime)
	sleep "$window"
	u1=$(awk '/^usage_usec/ { print $2 }' $cg/cpu.stat)
	c1=$(ctxt)
	t1=$(cut -d' ' -f1 /proc/uptime)
	echo "frozen_after=$(cat $cg/cgroup.freeze)"
	awk -v u0="$u0" -v u1="$u1" -v c0="$c0" -v c1="$c1" -v t0="$t0" -v t1="$t1" 'BEGIN {
		dt = t1 - t0; printf "idle_window_s=%.1f\nidle_cpu_percent_of_one_core=%.2f\nidle_context_switches_per_s=%.0f\n", dt, (u1 - u0) / 10000 / dt, (c1 - c0) / dt }'
fi
echo "top_pss_kib rss_kib anon_kib swap_kib pid name"
head -25 /tmp/deckard-slim-pss.$$
rm -f /tmp/deckard-slim-pss.$$
