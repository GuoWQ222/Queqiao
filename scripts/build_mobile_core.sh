#!/bin/bash
# Builds the mobile core (crates/queqiao-mobile) for Android: libqueqiao_mobile.so for arm64-v8a and x86_64 (the
# emulator), with the NDK (NDK_HOME, default /cpfs/user/guowenqi/android/android-ndk-r27c), API level 26.
# Usage: scripts/build_mobile_core.sh [out folder, default target/android-libs]. Prints CORE_BUILT <folder> or
# CORE_FAIL <target>.
set -u
cd "$(dirname "$0")/.." || exit 1
ndk=${NDK_HOME:-/cpfs/user/guowenqi/android/android-ndk-r27c}
target_dir=${CARGO_TARGET_DIR:-target}
mkdir -p "$target_dir"
out=${1:-$target_dir/android-libs}
api=26
bin=$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin
export PATH="$bin:$PATH"
for pair in "aarch64-linux-android arm64-v8a" "x86_64-linux-android x86_64"; do
    set -- $pair
    target=$1
    abi=$2
    var=$(echo "$target" | tr 'a-z-' 'A-Z_')
    export "CC_${target//-/_}=$bin/${target}${api}-clang"
    export "AR_${target//-/_}=$bin/llvm-ar"
    export "RANLIB_${target//-/_}=$bin/llvm-ranlib"
    export "CARGO_TARGET_${var}_LINKER=$bin/${target}${api}-clang"
    if ! cargo build --release -p queqiao-mobile --target "$target" > "$target_dir/mobile-$target.log" 2>&1; then
        grep -E "^error" -A 8 "$target_dir/mobile-$target.log" | head -30
        echo "CORE_FAIL $target"
        exit 1
    fi
    mkdir -p "$out/$abi"
    cp "$target_dir/$target/release/libqueqiao_mobile.so" "$out/$abi/"
done
echo "CORE_BUILT $out"
