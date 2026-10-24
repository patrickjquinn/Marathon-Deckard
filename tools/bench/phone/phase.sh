#!/bin/sh
# Usage: phase.sh bridges|hosts RUNS
# bridges: alternates Rust and C++ bridge runs (swap.sh cxx-bridge first).
# hosts: Rust host runs, then Python Waydroid runs, then restores Rust.
set -u
cd "$(dirname "$0")"
runs=$2
case $1 in
bridges)
	for run in $(seq "$runs"); do
		./bridge-run.sh "rust-$run" deckard-bridge deckard-bridge
		./bridge-run.sh "cxx-$run" deckardd deckardd
	done
	./swap.sh rust-bridge ;;
hosts)
	systemctl --user -M user@ stop deckard-session
	systemctl stop deckard-container
	for run in $(seq "$runs"); do ./host-run.sh "rust-$run" rust; done
	./swap.sh python-host
	for run in $(seq "$runs"); do ./host-run.sh "python-$run" python; done
	./swap.sh rust-host ;;
esac
echo "phase $1 finished"
