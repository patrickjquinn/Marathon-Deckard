#!/bin/sh
# Builds deckard-wm.jar (a dex jar run by app_process) with javac and d8.
# The stubs/ tree only satisfies javac; the device's framework classes are
# used at run time.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-$here/build}
sdk=${ANDROID_HOME:-$HOME/android-sdk}
bt=$sdk/build-tools/34.0.0
jar=$sdk/platforms/android-33/android.jar
export JAVA_HOME=${JAVA_HOME:-$(ls -d "$sdk"/jdk-17*)}
export PATH=$JAVA_HOME/bin:$PATH

rm -rf "$out"
mkdir -p "$out/stubs" "$out/classes" "$out/dex"
javac -source 1.8 -target 1.8 -Xlint:-options -bootclasspath "$jar" -d "$out/stubs" \
    $(find "$here/stubs" -name '*.java')
javac -source 1.8 -target 1.8 -Xlint:-options -bootclasspath "$jar:$bt/core-lambda-stubs.jar" \
    -cp "$out/stubs" -d "$out/classes" $(find "$here/src" -name '*.java')
java -cp "$bt/lib/d8.jar" com.android.tools.r8.D8 --release --min-api 33 --lib "$jar" \
    --classpath "$out/stubs" --output "$out/dex" $(find "$out/classes" -name '*.class')
(cd "$out/dex" && rm -f "$out/deckard-wm.jar" && zip -q "$out/deckard-wm.jar" classes.dex)
echo "$out/deckard-wm.jar"
