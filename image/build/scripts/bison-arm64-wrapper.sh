#!/bin/sh
# bison for an arm64 host. The prebuilt is relocatable only from
# .../linux-x86/bin and otherwise looks for its data under /nonexistent, so
# point it at common/bison next to the real binary.
d=$(dirname "$(readlink -f "$0")")
: "${BISON_PKGDATADIR:=$d/../../common/bison}"
export BISON_PKGDATADIR
exec "$d/bison.real" "$@"
