#!/bin/sh
# Usage: bench.sh bridges|hosts [RUNS]
# Copies phone/ to the HackberryPi, runs one phase there as a transient root
# unit (an SSH drop can't stop it half-way), waits, and fetches its output to
# results/. See docs/benchmarks.md for the setup each phase expects.
set -eu
phase=$1 runs=${2:-3}
here=$(cd "$(dirname "$0")" && pwd)
host=${BENCH_HOST:-root@192.168.68.79}
ssh() { sshpass -p "${BENCH_PASS:-027602}" command ssh -o StrictHostKeyChecking=no "$host" "$@"; }
ssh 'mkdir -p /run/deckard-bench/results'
sshpass -p "${BENCH_PASS:-027602}" scp -q "$here"/phone/* "$host":/run/deckard-bench/
if [ "$phase" = bridges ]; then
	ssh /run/deckard-bench/swap.sh cxx-bridge
fi
ssh "systemd-run --unit=deckard-bench-$phase --collect \
	-p StandardOutput=file:/run/deckard-bench/results/$phase.txt \
	-p StandardError=file:/run/deckard-bench/results/$phase.err \
	/run/deckard-bench/phase.sh $phase $runs"
while [ "$(ssh systemctl is-active deckard-bench-$phase)" = active ]; do sleep 30; done
mkdir -p "$here/results"
sshpass -p "${BENCH_PASS:-027602}" scp -q "$host":/run/deckard-bench/results/"$phase".txt "$here/results/"
python3 "$here/summarize.py" "$here/results/$phase.txt"
