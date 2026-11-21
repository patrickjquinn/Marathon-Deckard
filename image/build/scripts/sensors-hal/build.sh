#!/bin/sh
# Builds android.hardware.sensors@1.0-service.deckard outside Soong, with the
# flags Soong uses for a vendor arm64 binary on this tree (as
# camera-hal/build.sh does for the camera provider).
# usage: build.sh <lineage tree> <work dir>
set -eu
T=$(readlink -f "$1"); W=$(readlink -f "$2")
I=$T/out/soong/.intermediates
P=$T/out/target/product/deckard_arm64_only
CLANG=$T/prebuilts/clang/host/linux-arm64/clang-r596125
CXX=$CLANG/bin/clang++
SRC=$T/hardware/deckard/sensors
OBJ=$W/obj
mkdir -p "$OBJ"
CFLAGS="-nostdlibinc -Werror=implicit-function-declaration -D__BIONIC_DEPRECATED_PAGE_SIZE_MACRO -O2 -Wall -Wextra
 -Werror -Wno-unused-parameter -Wno-c99-designator -Wno-gnu-folding-constant -Wno-inconsistent-missing-override
 -Wno-reorder-init-list -Wno-sign-compare -Wno-missing-field-initializers -Wno-unused
 -DANDROID -DNDEBUG -UDEBUG -D__compiler_offsetof=__builtin_offsetof -D__ANDROID_UNAVAILABLE_SYMBOLS_ARE_WEAK__
 -faddrsig -fcolor-diagnostics -ffp-contract=off -fno-exceptions -fno-strict-aliasing -no-canonical-prefixes
 -ftrivial-auto-var-init=zero -ffunction-sections -fdata-sections -fno-short-enums -funwind-tables
 -fstack-protector-strong -Wa,--noexecstack -D_FORTIFY_SOURCE=3 -march=armv8-a -target aarch64-linux-android10000
 -DANDROID_STRICT -fPIC -fvisibility-inlines-hidden -D__ANDROID_VNDK__ -D__ANDROID_VENDOR__ -D__ANDROID_VENDOR_API__=202504
 -D__LIBC_API__=10000 -D__LIBM_API__=10000 -D__LIBDL_API__=10000 -std=gnu++2b -fno-rtti"
INC="-I$SRC
 -I$I/hardware/interfaces/sensors/1.0/android.hardware.sensors@1.0_genc++_headers/gen
 -I$I/system/libhidl/transport/base/1.0/android.hidl.base@1.0_genc++_headers/gen
 -I$I/system/libhidl/transport/manager/1.0/android.hidl.manager@1.0_genc++_headers/gen
 -I$T/system/libhidl/base/include -I$T/system/libhidl/transport/include -I$T/system/libfmq/base -I$T/system/libhwbinder/include
 -I$T/system/libbase/include -I$T/system/core/libutils/include -I$T/system/core/libutils/binder/include
 -I$T/system/core/libcutils/include -I$T/system/logging/liblog/include -I$T/system/core/libsystem/include
 -I$CLANG/android_libc++/platform/aarch64/include/c++/v1
 -I$CLANG/include/c++/v1
 -isystem $T/bionic/libc/include -isystem $T/bionic/libc/kernel/uapi/asm-arm64 -isystem $T/bionic/libc/kernel/uapi
 -isystem $T/bionic/libc/kernel/android/scsi -isystem $T/bionic/libc/kernel/android/uapi"
for f in Sensors service; do
	echo "CXX $SRC/$f.cpp"
	nice -n 10 $CXX -c $CFLAGS $INC -o "$OBJ/$f.o" "$SRC/$f.cpp"
done
V=$P/vendor/lib64; S=$P/system/lib64
OUT=$W/out/vendor/bin/hw/android.hardware.sensors@1.0-service.deckard
mkdir -p "$(dirname "$OUT")"
VLIBS="android.hardware.sensors@1.0 libbase libcutils libhidlbase libutils libc++"
SLIBS="liblog"
BIONIC="$I/bionic/libc/libc/android_vendor_arm64_armv8-a_shared/libc.so $I/bionic/libm/libm/android_vendor_arm64_armv8-a_shared/libm.so $I/bionic/libdl/libdl/android_vendor_arm64_armv8-a_shared/libdl.so"
echo "LD $OUT"
nice -n 10 $CXX $I/bionic/libc/crtbegin_dynamic/android_vendor_arm64_armv8-a/crtbegin_dynamic.o $OBJ/*.o \
	$CLANG/android_libc++/platform/aarch64/lib/libc++demangle.a \
	$CLANG/lib/clang/22/lib/linux/libclang_rt.ubsan_minimal-aarch64-android.a \
	-Wl,--start-group $CLANG/lib/clang/22/lib/linux/libclang_rt.builtins-aarch64-android.a -Wl,--end-group \
	$(for l in $VLIBS; do echo "$V/$l.so"; done) $(for l in $SLIBS; do echo "$S/$l.so"; done) $BIONIC \
	$I/bionic/libc/crtend_android/android_vendor_arm64_armv8-a/crtend_android.o \
	-o "$OUT.unstripped" -target aarch64-linux-android10000 -Wl,-z,noexecstack -Wl,-z,relro -Wl,-z,now \
	-Wl,--build-id=md5 -Wl,--fatal-warnings -Wl,--no-undefined-version -Wl,--exclude-libs,libgcc.a \
	-Wl,--exclude-libs,libunwind_llvm.a -Wl,--exclude-libs,libunwind.a -fuse-ld=lld -Wl,--icf=safe \
	-Wl,--no-undefined -Wl,-z,separate-code -Wl,-z,separate-loadable-segments -Wl,-z,max-page-size=4096 \
	-Wl,--pack-dyn-relocs=android+relr -pie -nostdlib -Bdynamic -Wl,--gc-sections -Wl,-z,nocopyreloc \
	-Wl,--exclude-libs=libclang_rt.builtins-aarch64-android.a -Wl,--exclude-libs=libclang_rt.ubsan_minimal-aarch64-android.a \
	-Wl,-dynamic-linker,/system/bin/linker64
$CLANG/bin/llvm-strip --strip-all -o "$OUT" "$OUT.unstripped"
