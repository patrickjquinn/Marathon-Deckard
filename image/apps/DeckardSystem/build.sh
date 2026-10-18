#!/bin/sh
# Builds DeckardSystem.apk, signed with AOSP's platform test key (the image's
# test-keys build trusts it as android.uid.system). The key is AOSP's public
# build/target/product/security/platform, kept outside git.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
export PLATFORM_KEY=${PLATFORM_KEY:-$HOME/.marathon-secrets/deckard/aosp-test-keys/platform}
exec "$here/../../tools/build-apk.sh" "$here" "$@"
