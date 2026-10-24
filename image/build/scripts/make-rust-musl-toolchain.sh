#!/bin/bash
# Assemble Rust for the arm64 host in the layout Soong expects
# (prebuilts/rust/linux-arm64/<ver>).
#   usage: make-rust-musl-toolchain.sh <toolchains dir> <lineage tree>
#
# Soong builds proc-macro crates as musl shared objects, which a glibc rustc
# cannot load, so this uses the official aarch64-unknown-linux-musl rustc.
# The host has no /lib/ld-musl-aarch64.so.1: bin/rustc starts rustc.bin
# through the musl loader that prebuilts/build-tools ships.
set -euo pipefail
P=$(readlink -f "$1") T=$(readlink -f "$2")
V=${RUST_VER:-1.88.0}
OUT=$P/rust-$V-musl
rm -rf "$OUT"
work=$(mktemp -d -p "$P"); trap 'rm -rf "$work"' EXIT
tar -C "$work" -xf "$P/dl/rust-$V-aarch64-unknown-linux-musl.tar.xz"
"$work/rust-$V-aarch64-unknown-linux-musl/install.sh" --prefix="$OUT" \
	--components=rustc,rustfmt-preview,rust-std-aarch64-unknown-linux-musl --disable-ldconfig >/dev/null
cp "$T/prebuilts/build-tools/linux-arm64/lib64/libc_musl.so" "$OUT/lib/libc.so"
cp "$P/clang/clang-r596125/lib/aarch64-unknown-linux-musl/libunwind.so" "$OUT/lib/libgcc_s.so.1"
# The std sources are the same for every host: use the x86 toolchain's.
ln -sfn "$T/prebuilts/rust/linux-x86/$V/lib/rustlib/src" "$OUT/lib/rustlib/src"
for b in rustc rustdoc rustfmt; do
	mv "$OUT/bin/$b" "$OUT/bin/$b.bin"
	cat > "$OUT/bin/$b" <<'EOS'
#!/bin/sh
d=$(dirname "$(readlink -f "$0")")
b=$(basename "$0")
if [ "$b" != rustfmt ]; then
	case " $* " in *" --sysroot "*|*" --sysroot="*) ;; *) set -- --sysroot "$d/.." "$@";; esac
fi
exec "$d/../lib/libc.so" --library-path "$d/../lib" "$d/$b.bin" "$@"
EOS
	chmod +x "$OUT/bin/$b"
done
"$OUT/bin/rustc" --version
"$OUT/bin/rustfmt" --version
