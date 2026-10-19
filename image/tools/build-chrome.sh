#!/bin/sh
# Builds the chrome-and-theme pieces and stages them in image/overlay:
# the framework, palette and PermissionController overlays,
# DeckardHome and deckard-wm.
# FRAMEWORK_RES is the Waydroid image's /system/framework/framework-res.apk
# (the caption overlay names private framework ids).
set -eu
image=$(cd "$(dirname "$0")/.." && pwd)
: "${FRAMEWORK_RES:?set FRAMEWORK_RES to the image's framework-res.apk}"
export FRAMEWORK_RES
build=${BUILD:-${XDG_CACHE_HOME:-$HOME/.cache}/deckard-chrome-build}
ov=$image/overlay/system

python3 "$image/vendor_deckard/overlay/DeckardPalette/gen-palette.py" "$@"
for name in DeckardFrameworkRes DeckardPalette DeckardPermissionControllerRes; do
    "$image/tools/build-apk.sh" "$image/vendor_deckard/overlay/$name" "$build/$name" >/dev/null
    install -Dm644 "$build/$name/$name.apk" "$ov/product/overlay/$name/$name.apk"
done
# DeckardHome shares the system uid with DeckardSystem: platform key.
FRAMEWORK_RES= PLATFORM_KEY=${PLATFORM_KEY:-$HOME/.marathon-secrets/deckard/aosp-test-keys/platform} \
    "$image/tools/build-apk.sh" "$image/apps/DeckardHome" "$build/DeckardHome" >/dev/null
install -Dm644 "$build/DeckardHome/DeckardHome.apk" "$ov/product/app/DeckardHome/DeckardHome.apk"
"$image/vendor_deckard/deckard-wm/build.sh" "$build/deckard-wm" >/dev/null
install -Dm644 "$build/deckard-wm/deckard-wm.jar" "$ov/framework/deckard-wm.jar"
find "$ov/product/overlay" "$ov/product/app/DeckardHome" "$ov/framework/deckard-wm.jar" -type f
