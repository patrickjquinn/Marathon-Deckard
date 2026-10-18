#!/bin/sh
# Builds an app under image/apps without Gradle: javac, d8, aapt, zipalign,
# apksigner. Usage: build-apk.sh <app dir> [out dir]. The app dir holds
# AndroidManifest.xml, src/ and optionally res/; the APK is named after it.
# Without src/ it builds a resource-only APK (an overlay). Resources that
# name private framework resources (@*android:...) link against the device's
# framework-res.apk, given as FRAMEWORK_RES. Classes under stubs/ (hidden
# framework APIs) are compiled against but not packaged. PLATFORM_KEY (a path
# without the .pk8/.x509.pem suffix) signs with that key instead of Deckard's.
# The signing keys live outside git in ~/.marathon-secrets/deckard.
set -eu
app=$(cd "$1" && pwd)
name=$(basename "$app")
out=${2:-$app/build}
sdk=${ANDROID_HOME:-$HOME/android-sdk}
bt=$sdk/build-tools/34.0.0
jar=$sdk/platforms/android-33/android.jar
export JAVA_HOME=${JAVA_HOME:-$(ls -d "$sdk"/jdk-17*)}
export PATH=$JAVA_HOME/bin:$PATH
aapt=${AAPT:-$sdk/tools/aapt}
zipalign=${ZIPALIGN:-$sdk/tools/zipalign}
keystore=${DECKARD_KEYSTORE:-$HOME/.marathon-secrets/deckard/deckard.keystore}
[ -n "${PLATFORM_KEY:-}" ] || [ -f "$keystore" ] || { echo "no signing key at $keystore" >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out/classes" "$out/dex"
if [ -d "$app/src" ]; then
    cp=
    if [ -d "$app/stubs" ]; then
        mkdir -p "$out/stubs"
        javac -source 1.8 -target 1.8 -Xlint:-options -bootclasspath "$jar" -d "$out/stubs" \
            $(find "$app/stubs" -name '*.java')
        cp="$out/stubs"
    fi
    javac -source 1.8 -target 1.8 -Xlint:-options -Xlint:deprecation \
        -bootclasspath "$jar:$bt/core-lambda-stubs.jar" ${cp:+-cp "$cp"} -d "$out/classes" \
        $(find "$app/src" -name '*.java')
    "$bt/d8" --release --min-api 33 --lib "$jar" ${cp:+--classpath "$cp"} --output "$out/dex" \
        $(find "$out/classes" -name '*.class')
fi
res=
[ -d "$app/res" ] && res="-S $app/res"
"$aapt" package -f -M "$app/AndroidManifest.xml" $res -I "${FRAMEWORK_RES:-$jar}" -F "$out/unsigned.apk"
[ -f "$out/dex/classes.dex" ] && (cd "$out/dex" && zip -q "$out/unsigned.apk" classes.dex)
"$zipalign" -f -p 4 "$out/unsigned.apk" "$out/aligned.apk"
if [ -n "${PLATFORM_KEY:-}" ]; then
    "$bt/apksigner" sign --key "$PLATFORM_KEY.pk8" --cert "$PLATFORM_KEY.x509.pem" \
        --out "$out/$name.apk" "$out/aligned.apk"
else
    "$bt/apksigner" sign --ks "$keystore" --ks-pass "file:$keystore.pass" --ks-key-alias deckard \
        --out "$out/$name.apk" "$out/aligned.apk"
fi
rm -f "$out/unsigned.apk" "$out/aligned.apk" "$out/$name.apk.idsig"
echo "$out/$name.apk"
