#!/bin/bash
# Fetch the arm64 host toolchains for lineage-23.2 into $1 (default
# /mnt/deckard-build/toolchains) and record each source, revision and sha256
# in SOURCES.txt.
#
# lineage-23.2 asks for clang-r563880c (clang 21), Rust 1.88.0, Go 1.24 and
# JDK 21. Google publishes no arm64 clang 21, so this takes clang-r596125
# (22.0.2) and Go 1.25.9 from the same AOSP release branch.
set -euo pipefail
DEST=$(readlink -f "${1:-/mnt/deckard-build/toolchains}")
AOSP=https://android.googlesource.com
LLVM_BRANCH=mirror-google-llvm-r614150-release
CLANG_VER=clang-r596125
RUST_VER=1.88.0
mkdir -p "$DEST/dl"; cd "$DEST"
log() { echo "$*" | tee -a SOURCES.txt; }
: > SOURCES.txt
log "# fetched $(date -u +%FT%TZ) on $(uname -m)"

if [ ! -d clang/$CLANG_VER ]; then
	git clone -q --filter=blob:none --no-checkout -b $LLVM_BRANCH \
		$AOSP/platform/prebuilts/clang/host/linux-arm64 clang
	git -C clang sparse-checkout set --no-cone /$CLANG_VER/ /Android.bp
	git -C clang checkout -q $LLVM_BRANCH
fi
log "clang $AOSP/platform/prebuilts/clang/host/linux-arm64 $LLVM_BRANCH $(git -C clang rev-parse HEAD) $CLANG_VER"

[ -d go ] || git clone -q --depth=1 -b $LLVM_BRANCH $AOSP/platform/prebuilts/go/linux-arm64 go
log "go $AOSP/platform/prebuilts/go/linux-arm64 $LLVM_BRANCH $(git -C go rev-parse HEAD) $(head -1 go/VERSION)"

for f in rust-$RUST_VER-aarch64-unknown-linux-musl.tar.xz; do
	url=https://static.rust-lang.org/dist/$f
	[ -f dl/$f ] || curl -sfL -o dl/$f "$url"
	(cd dl && curl -sfL "$url.sha256" | awk '{print $1"  '"$f"'"}' | sha256sum -c --quiet)
	log "rust $url sha256=$(sha256sum dl/$f | cut -d' ' -f1)"
done

api='https://api.adoptium.net/v3/assets/latest/21/hotspot?architecture=aarch64&image_type=jdk&os=linux&vendor=eclipse'
read -r JURL JSUM JNAME < <(curl -sfL "$api" | python3 -I -c '
import json, sys
p = json.load(sys.stdin)[0]["binary"]["package"]
print(p["link"], p["checksum"], p["name"])')
[ -f dl/$JNAME ] || curl -sfL -o dl/$JNAME "$JURL"
(cd dl && echo "$JSUM  $JNAME" | sha256sum -c --quiet)
[ -d jdk21 ] || { mkdir jdk21; tar -C jdk21 --strip-components=1 -xzf dl/$JNAME; }
log "jdk $JURL sha256=$JSUM"
