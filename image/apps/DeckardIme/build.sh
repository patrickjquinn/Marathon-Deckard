#!/bin/sh
# Runs ContentTypeTest on the host JVM, then builds DeckardIme.apk.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
sdk=${ANDROID_HOME:-$HOME/android-sdk}
bt=$sdk/build-tools/34.0.0
jar=$sdk/platforms/android-33/android.jar
export JAVA_HOME=${JAVA_HOME:-$(ls -d "$sdk"/jdk-17*)}
export PATH=$JAVA_HOME/bin:$PATH
test=$(mktemp -d)
trap 'rm -rf "$test"' EXIT
javac -source 1.8 -target 1.8 -Xlint:-options -bootclasspath "$jar:$bt/core-lambda-stubs.jar" \
    -d "$test" "$here/src/org/marathonos/deckard/ime/ContentType.java" \
    $(find "$here/test" -name '*.java')
java -cp "$test" org.marathonos.deckard.ime.ContentTypeTest
exec "$here/../../tools/build-apk.sh" "$here" "$@"
