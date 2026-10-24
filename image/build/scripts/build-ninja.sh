#!/bin/bash
# Build AOSP's ninja for a 16K-page arm64 host.
#   usage: build-ninja.sh <toolchains dir>
# prebuilts/build-tools/linux-arm64/bin/ninja links a jemalloc built for 4K
# pages, which aborts ("Unsupported system page size") on a 16K kernel. This
# builds the same fork from source with the system C++ compiler and glibc
# malloc. The revision in prebuilts/build-tools/manifest.xml is not public;
# build-tools-release is the nearest published one.
set -euo pipefail
P=$(readlink -f "$1")
REV=5e0cdc53bb22eaed25541aade2ec9640934a88b1
S=$P/src/ninja
[ -d "$S" ] || git clone -q -b build-tools-release https://android.googlesource.com/platform/external/ninja "$S"
git -C "$S" checkout -q $REV
mkdir -p "$P/ninja/gen/build"
"$S/src/inline.sh" kBrowsePy < "$S/src/browse.py" > "$P/ninja/gen/build/browse_py.h"
srcs="build build_log clean clparser debug_flags depfile_parser deps_log disk_interface
  dyndep dyndep_parser edit_distance eval_env graph graphviz lexer line_printer
  manifest_chunk_parser manifest_parser metrics parser proto state status
  thread_pool util version browse subprocess-posix ninja"
/usr/bin/g++ -O2 -std=gnu++17 -Wno-deprecated -fno-exceptions -DNINJA_PYTHON='"python"' \
	-DNINJA_HAVE_BROWSE -DUSE_PPOLL -I"$P/ninja/gen" \
	$(for s in $srcs; do echo "$S/src/$s.cc"; done) -lpthread -o "$P/ninja/ninja"
"$P/ninja/ninja" --version
