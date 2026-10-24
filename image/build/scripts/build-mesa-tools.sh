#!/bin/bash
# Build Mesa's build-time compilers (mesa_clc, asahi_clc, panfrost_compile,
# vtn_bindgen2) for an arm64 host from the tree's own external/mesa3d.
#   usage: build-mesa-tools.sh <toolchains dir> <lineage tree>
# Waydroid builds Mesa with -Dmesa-clc=system -Dprecomp-compiler=system and
# ships x86_64 builds of these in prebuilts/mesa-tools. Needs Fedora's
# llvm-devel clang-devel libclc-devel spirv-llvm-translator-devel
# spirv-tools-devel python3-pycparser glslang (glslangValidator).
set -euo pipefail
P=$(readlink -f "$1") T=$(readlink -f "$2")
B=$P/mesa-tools-build OUT=$P/mesa-tools
meson setup --wipe "$B" "$T/external/mesa3d" \
	-Dbuildtype=release -Dplatforms= -Dglx=disabled -Degl=disabled \
	-Dgbm=disabled -Dopengl=false -Dgles1=disabled -Dgles2=disabled \
	-Dgallium-drivers=asahi,panfrost -Dvulkan-drivers= -Dllvm=enabled -Dshared-llvm=enabled \
	-Dxmlconfig=disabled -Dvalgrind=disabled -Dlibunwind=disabled \
	-Dmesa-clc=enabled -Dprecomp-compiler=enabled | tail -40
ninja -C "$B" -j"${JOBS:-2}" src/compiler/clc/mesa_clc src/compiler/spirv/vtn_bindgen2 \
	src/asahi/clc/asahi_clc src/panfrost/clc/panfrost_compile
mkdir -p "$OUT/bin"
for t in compiler/clc/mesa_clc compiler/spirv/vtn_bindgen2 asahi/clc/asahi_clc panfrost/clc/panfrost_compile; do
	cp "$B/src/$t" "$OUT/bin/"
done
ls -la "$OUT/bin"
