#!/bin/sh
# Builds an app under image/apps without Gradle: javac, d8, aapt, zipalign,
# apksigner. Usage: build-apk.sh <app dir> [out dir]. The app dir holds
# AndroidManifest.xml, src/ and optionally res/; the APK is named after it.
# The signing key lives outside git in ~/.marathon-secrets/deckard.
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
[ -f "$keystore" ] || { echo "no signing key at $keystore" >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out/classes" "$out/dex"
javac -source 1.8 -target 1.8 -Xlint:-options -Xlint:deprecation \
    -bootclasspath "$jar:$bt/core-lambda-stubs.jar" -d "$out/classes" $(find "$app/src" -name '*.java')
"$bt/d8" --release --min-api 33 --lib "$jar" --output "$out/dex" $(find "$out/classes" -name '*.class')
res=
[ -d "$app/res" ] && res="-S $app/res"
"$aapt" package -f -M "$app/AndroidManifest.xml" $res -I "$jar" -F "$out/unsigned.apk"
(cd "$out/dex" && zip -q "$out/unsigned.apk" classes.dex)
"$zipalign" -f -p 4 "$out/unsigned.apk" "$out/aligned.apk"
"$bt/apksigner" sign --ks "$keystore" --ks-pass "file:$keystore.pass" --ks-key-alias deckard \
    --out "$out/$name.apk" "$out/aligned.apk"
rm -f "$out/unsigned.apk" "$out/aligned.apk" "$out/$name.apk.idsig"
echo "$out/$name.apk"
