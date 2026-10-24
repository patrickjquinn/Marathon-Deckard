#!/bin/sh
# Usage: bridge-run.sh LABEL UNIT EXE
# Restarts the bridge unit (startup time), lets it settle, samples it idle
# for 120 s, then across the latency workload. Android must be booted, with
# Markor's search field focused and the OSK shown.
set -u
label=$1 unit=$2 exe=$3
cd "$(dirname "$0")"
systemctl --user -M user@ stop deckard-bridge deckardd
sleep 3
python3 bench.py startup "$label" session org.marathonos.Deckard 3 \
	"systemctl --user -M user@ start $unit"
sleep 30
pid=$(pidof "$exe")
./sample.sh "$label-idle" 120 "$pid"
rm -f done
./sample.sh "$label-work" until:done "$pid" &
python3 bench.py latency "$label" mpris 20
python3 bench.py latency "$label" notify 20
python3 bench.py latency "$label" ime 20
touch done
wait
