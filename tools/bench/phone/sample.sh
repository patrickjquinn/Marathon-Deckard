#!/bin/sh
# Usage: sample.sh LABEL SECONDS|until:FILE PID...
# One line per PID: memory at the end of the window, and CPU time and
# context switches over it, summed over the process's threads. until:FILE
# ends the window when FILE appears.
label=$1 secs=$2
shift 2
counters() {
	cat /proc/$1/task/*/schedstat 2>/dev/null | awk '{ns += $1; sl += $3} END {printf "%d %d ", ns, sl}'
	cat /proc/$1/task/*/status 2>/dev/null | awk '/^voluntary_ctxt/ {v += $2} /^nonvoluntary_ctxt/ {n += $2} END {printf "%d %d ", v, n}'
	awk '{print $14 + $15}' /proc/$1/stat
}
before=
for p in "$@"; do before="$before $p:$(counters $p | tr ' ' :)"; done
start=$(date +%s.%N)
case $secs in
until:*) until [ -e "${secs#until:}" ]; do sleep 0.2; done ;;
*) sleep "$secs" ;;
esac
secs=$(echo "$(date +%s.%N) - $start" | bc)
for entry in $before; do
	p=${entry%%:*}
	[ -d /proc/$p ] || { echo "$label pid=$p gone"; continue; }
	set -- $(echo "${entry#*:}" | tr : ' ') $(counters $p)
	mem=$(awk '/^Rss:/ {r = $2} /^Pss:/ {p = $2} /^Private_Clean:/ {c = $2} /^Private_Dirty:/ {d = $2}
		END {printf "rss_kb=%d pss_kb=%d uss_kb=%d private_dirty_kb=%d", r, p, c + d, d}' /proc/$p/smaps_rollup)
	threads=$(awk '/^Threads:/ {print $2}' /proc/$p/status)
	echo "$label pid=$p comm=$(cat /proc/$p/comm) window_s=$secs $mem threads=$threads" \
		"$(awk -v s="$secs" -v a="$*" 'BEGIN {split(a, x, " ");
		printf "cpu_ms=%.2f tick_ms=%d slices_s=%.2f vol_s=%.2f invol_s=%.2f",
		(x[6] - x[1]) / 1e6, (x[10] - x[5]) * 10, (x[7] - x[2]) / s, (x[8] - x[3]) / s, (x[9] - x[4]) / s}')"
done
