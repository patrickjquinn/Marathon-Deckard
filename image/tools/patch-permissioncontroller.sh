#!/bin/sh
# Builds Deckard's PermissionController: the image's own APK with
# GrantPermissionsActivity drawing through MarathonGrantView
# (image/apps/PermissionControllerPatch, docs/permissions.md) instead of the
# handheld dialog, signed again with the platform key it already carries.
# Usage: patch-permissioncontroller.sh <stock PermissionController.apk> <out.apk>
# The stock APK must be the one the patch was made for (STOCK_SHA256).
set -eu
stock=$1
out=$2
here=$(cd "$(dirname "$0")" && pwd)
src=$here/../apps/PermissionControllerPatch
sdk=${ANDROID_HOME:-$HOME/android-sdk}
bt=$sdk/build-tools/34.0.0
jar=$sdk/platforms/android-33/android.jar
smali=${SMALI_DIR:-$sdk/tools/smali}
zipalign=${ZIPALIGN:-$sdk/tools/zipalign}
key=${PLATFORM_KEY:-$HOME/.marathon-secrets/deckard/aosp-test-keys/platform}
export JAVA_HOME=${JAVA_HOME:-$(ls -d "$sdk"/jdk-17*)}
export PATH=$JAVA_HOME/bin:$PATH
# LineageOS 20 TQ3A.230901.001, Waydroid system image 1790477549.
want=${STOCK_SHA256:-8d9d4f73ebebfb9adfc72e472e420d2a67e5a081c0f943d7df062e43d9aaed13}

have=$(sha256sum "$stock" | cut -d' ' -f1)
[ "$have" = "$want" ] || { echo "$stock is not the PermissionController this patch is for ($have)" >&2; exit 1; }

# Under $HOME: the zipalign wrapper runs in a container that mounts only $HOME.
work=$(mktemp -d "$HOME/.cache/pcpatch.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/stubs" "$work/classes" "$work/dex2"
javac -source 1.8 -target 1.8 -Xlint:-options -bootclasspath "$jar" -d "$work/stubs" $(find "$src/stubs" -name '*.java')
javac -source 1.8 -target 1.8 -Xlint:-options -bootclasspath "$jar:$bt/core-lambda-stubs.jar" -cp "$work/stubs" \
    -d "$work/classes" $(find "$src/src" -name '*.java')
"$bt/d8" --release --min-api 33 --lib "$jar" --classpath "$work/stubs" --output "$work/dex2" \
    $(find "$work/classes" -name '*.class')

unzip -q -o "$stock" classes.dex -d "$work"
java -jar "$smali/baksmali.jar" d -a 33 "$work/classes.dex" -o "$work/smali"
cargo run --locked --manifest-path "$here/../../Cargo.toml" -p deckard-image-tools -- \
    permission-view \
    --input "$work/smali/com/android/permissioncontroller/permission/ui/GrantPermissionsActivity.smali" \
    --out "$work/smali/com/android/permissioncontroller/permission/ui/GrantPermissionsActivity.smali"
java -jar "$smali/smali.jar" a -a 33 "$work/smali" -o "$work/apk-classes.dex"

cp "$stock" "$work/unsigned.apk"
chmod u+w "$work/unsigned.apk"
zip -q -d "$work/unsigned.apk" 'META-INF/CERT.SF' 'META-INF/CERT.RSA' 'META-INF/MANIFEST.MF' || true
mkdir -p "$work/z"
cp "$work/apk-classes.dex" "$work/z/classes.dex"
cp "$work/dex2/classes.dex" "$work/z/classes2.dex"
(cd "$work/z" && zip -q -0 "$work/unsigned.apk" classes.dex classes2.dex)
"$zipalign" -f -p 4 "$work/unsigned.apk" "$work/aligned.apk"
"$bt/apksigner" sign --key "$key.pk8" --cert "$key.x509.pem" --out "$out" "$work/aligned.apk"
rm -f "$out.idsig"
"$bt/apksigner" verify "$out"
echo "$out $(sha256sum "$out" | cut -d' ' -f1)"
