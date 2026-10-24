#!/bin/bash
# Link the arm64 toolchains into a lineage-23.2 tree (symlinks only).
#   usage: place-arm64-prebuilts.sh <lineage tree> <toolchains dir>
# The toolchains dir is what fetch-arm64-prebuilts.sh and
# make-rust-musl-toolchain.sh produce.
set -euo pipefail
T=$(readlink -f "$1") P=$(readlink -f "$2")
ln_() { mkdir -p "$(dirname "$2")"; ln -sfn "$1" "$2"; }
c=$T/prebuilts/clang/host/linux-arm64
ln_ "$P/clang/clang-r596125" "$c/clang-r596125"
ln_ clang-r596125 "$c/clang-stable"
# The clang Soong plugin in linux-x86 resolves the device runtimes, libc++
# and the llvm build tools under its own directory; the arm64 package carries
# the same Android target runtimes.
ln_ "$P/clang/clang-r596125" "$T/prebuilts/clang/host/linux-x86/clang-r596125"
ln_ "$P/go" "$T/prebuilts/go/linux-arm64"
ln_ "$P/jdk21" "$T/prebuilts/jdk/jdk21/linux-arm64"
ln_ linux-x86 "$T/prebuilts/jdk/jdk8/linux-arm64"   # only its jars are used
ln_ "$P/rust-1.88.0-musl" "$T/prebuilts/rust/linux-arm64/1.88.0"
# ninja from build-ninja.sh replaces the 4K-page jemalloc prebuilt.
b=$T/prebuilts/build-tools/linux-arm64/bin
[ -L "$b/ninja" ] || mv "$b/ninja" "$b/ninja.jemalloc4k"
ln -sfn "$P/ninja/ninja" "$b/ninja"
# bison: the wrapper sets BISON_PKGDATADIR for genrules that run it directly.
if ! head -c2 "$b/bison" | grep -q '#!'; then mv "$b/bison" "$b/bison.real"; fi
cp "$(dirname "$0")/bison-arm64-wrapper.sh" "$b/bison"
# Mesa's build-time compilers from build-mesa-tools.sh, where Waydroid links
# its x86_64 prebuilts/mesa-tools for linux-x86.
for t in mesa_clc asahi_clc panfrost_compile vtn_bindgen2; do
	ln_ "$P/mesa-tools/bin/$t" "$T/prebuilts/build-tools/path/linux-arm64/$t"
done
