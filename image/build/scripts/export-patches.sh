#!/bin/bash
# Export each project's commits since its deckard/base tag into ../patches.
#   usage: export-patches.sh <lineage tree> <project>...
set -euo pipefail
T=$(readlink -f "$1"); shift; P=$(readlink -f "$(dirname "$0")/../patches")
for proj in "$@"; do
  d=$P/$(echo "$proj" | tr / _)
  rm -rf "$d"; mkdir -p "$d"
  git -C "$T/$proj" format-patch -q --zero-commit --no-signature --no-numbered \
    -o "$d" deckard/base..HEAD
  ls "$d"
done
