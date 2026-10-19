#!/bin/sh
# Lays out microG's signed release APKs as privileged system apps under an
# overlay root: install.sh <GmsCore apk> <Companion apk> <overlay root> <abi>.
# The APKs keep microG's signature (the framework only lets microG-signed
# copies spoof Google's), so nothing in them changes; their native code for
# <abi> is unpacked beside them, as Android doesn't extract a system app's.
set -eu
gms=$1 vending=$2 root=$3 abi=$4
case "$abi" in
arm64-v8a) isa=arm64 ;;
x86_64) isa=x86_64 ;;
*) echo "install.sh: unknown ABI $abi" >&2; exit 2 ;;
esac
here=$(cd "$(dirname "$0")" && pwd)
pinned() {
	want=$(awk -v f="$(basename "$1")" '$2 == f { print $1 }' "$here/SHA256SUMS")
	[ -n "$want" ] && [ "$(sha256sum "$1" | cut -d ' ' -f 1)" = "$want" ] ||
		{ echo "install.sh: $1 is not a pinned microG release" >&2; exit 1; }
}
pinned "$gms"
pinned "$vending"
app=$root/system/priv-app/GmsCore
rm -rf "$app" "$root/system/priv-app/FakeStore"
install -Dm644 "$gms" "$app/GmsCore.apk"
install -Dm644 "$vending" "$root/system/priv-app/FakeStore/FakeStore.apk"
mkdir -p "$app/lib/$isa"
unzip -q -j -o "$gms" "lib/$abi/*.so" -d "$app/lib/$isa"
chmod 644 "$app/lib/$isa"/*.so
