#!/bin/bash
# Build a lineage-23.2 Waydroid target on an aarch64 host.
#   usage: build.sh <lineage tree> <log file> <m arguments...>
#   env:   LUNCH (default lineage_waydroid_arm64_only-bp4a-userdebug)
#          JOBS  (default 4)
# Example: build.sh /mnt/deckard-build/lineage /tmp/sys.log systemimage
set -uo pipefail
T=$(readlink -f "$1") LOG=$2; shift 2
LUNCH=${LUNCH:-lineage_waydroid_arm64_only-bp4a-userdebug}
JOBS=${JOBS:-4}
mkdir -p "$T/../tmp"
# env -i: interactive shell functions (a grep wrapper, ...) must not reach
# the build's scripts.
env -i HOME="$HOME" USER="$USER" TERM=dumb PATH=/usr/local/bin:/usr/bin:/bin \
	TMPDIR="$(readlink -f "$T/../tmp")" \
	bash --noprofile --norc -c '
cd "$0"
export OUT_DIR=out
export SKIP_ABI_CHECKS=true         # no arm64 header-abi-dumper
export SOONG_DISABLE_CLIPPY=true    # clippy is not in the arm64 Rust toolchain
export THINLTO_USE_MLGO=false       # clang-r596125 for arm64 has no MLGO model
export LLVM_PREBUILTS_VERSION=clang-r596125
export LLVM_RELEASE_VERSION=22
export LLVM_BINDGEN_PREBUILTS_VERSION=clang-r596125
source build/envsetup.sh >/dev/null 2>&1
lunch "$1" || exit 1
log=$2; shift 2
start=$(date +%s)
m "$@" > "$log" 2>&1
rc=$?
echo "exit=$rc seconds=$(( $(date +%s) - start ))" >> "$log"
exit $rc
' "$T" "$LUNCH" "$LOG" -j"$JOBS" "$@"
