#!/bin/sh
# Builds the chrome-and-theme pieces and stages them in image/overlay:
# the framework, palette and PermissionController overlays,
# and DeckardHome.
# FRAMEWORK_RES is the Waydroid image's /system/framework/framework-res.apk
# (the caption overlay names private framework ids).
set -eu
image=$(cd "$(dirname "$0")/.." && pwd)
: "${FRAMEWORK_RES:?set FRAMEWORK_RES to the image's framework-res.apk}"
export FRAMEWORK_RES
build=${BUILD:-${XDG_CACHE_HOME:-$HOME/.cache}/deckard-chrome-build}
ov=$image/overlay/system

palette_out=default
palette_theme=default
for argument in "$@"; do
    case "$argument" in
        --out | --out=*) palette_out=explicit ;;
        --mcolors | --mcolors=*) palette_theme=explicit ;;
    esac
done
if [ "$palette_out" = default ]; then
    set -- --out "$image/vendor_deckard/overlay/DeckardPalette/res/values/colors.xml" "$@"
fi
if [ "$palette_theme" = default ]; then
    set -- --mcolors "${DECKARD_MCOLORS:-$HOME/Developer/Marathon-Shell/shell/qml/theme/MColors.qml}" "$@"
fi
cargo run --locked --manifest-path "$image/../Cargo.toml" -p deckard-image-tools -- palette "$@"
for name in DeckardFrameworkRes DeckardPalette DeckardPermissionControllerRes; do
    "$image/tools/build-apk.sh" "$image/vendor_deckard/overlay/$name" "$build/$name" >/dev/null
    install -Dm644 "$build/$name/$name.apk" "$ov/product/overlay/$name/$name.apk"
done
# DeckardHome shares the system uid with DeckardSystem: platform key.
FRAMEWORK_RES= PLATFORM_KEY=${PLATFORM_KEY:-$HOME/.marathon-secrets/deckard/aosp-test-keys/platform} \
    "$image/tools/build-apk.sh" "$image/apps/DeckardHome" "$build/DeckardHome" >/dev/null
install -Dm644 "$build/DeckardHome/DeckardHome.apk" "$ov/product/app/DeckardHome/DeckardHome.apk"
find "$ov/product/overlay" "$ov/product/app/DeckardHome" -type f
