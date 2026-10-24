#!/bin/bash
# Log the RSS of the build's biggest processes and system memory every 30s.
#   usage: memlog.sh <log file>
while :; do
	{ date +%T; free -m | awk 'NR>1{print "  "$1, "used="$3, "avail="$7}';
	  ps -eo rss=,comm= --sort=-rss | awk 'NR<=4{printf "  %s %d MB\n", $2, $1/1024}'; } >> "$1"
	sleep 30
done
