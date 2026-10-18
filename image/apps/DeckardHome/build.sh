#!/bin/sh
# Builds DeckardHome.apk with AOSP's platform test key, so it shares
# DeckardSystem's uid and process (see DeckardSystem/build.sh).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
export PLATFORM_KEY=${PLATFORM_KEY:-$HOME/.marathon-secrets/deckard/aosp-test-keys/platform}
exec "$here/../../tools/build-apk.sh" "$here" "$@"
