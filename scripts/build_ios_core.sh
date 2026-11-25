#!/bin/bash
# Builds the mobile core (crates/queqiao-mobile) for iOS on a Mac: static libraries for iPhones (aarch64-apple-ios) and
# the Simulator on Apple silicon (aarch64-apple-ios-sim), packed with include/queqiao_mobile.h into
# mobile/ios/build/QueqiaoCore.xcframework for the Xcode project (mobile/ios/project.yml).
# Prints CORE_BUILT <xcframework> or CORE_FAIL <target>.
set -u
cd "$(dirname "$0")/.." || exit 1
target_dir=${CARGO_TARGET_DIR:-target}
mkdir -p "$target_dir"
export IPHONEOS_DEPLOYMENT_TARGET=15.0
args=()
for target in aarch64-apple-ios aarch64-apple-ios-sim; do
    log=$target_dir/ios-$target.log
    if ! cargo rustc --release -p queqiao-mobile --lib --crate-type staticlib --target "$target" > "$log" 2>&1; then
        grep -E "^(error|warning: unused)" -A 8 "$log" | head -40
        tail -20 "$log"
        echo "CORE_FAIL $target"
        exit 1
    fi
    args+=(-library "$target_dir/$target/release/libqueqiao_mobile.a" -headers crates/queqiao-mobile/include)
done
out=mobile/ios/build/QueqiaoCore.xcframework
rm -rf "$out"
mkdir -p mobile/ios/build
xcodebuild -create-xcframework "${args[@]}" -output "$out" > "$target_dir/ios-xcframework.log" 2>&1 \
    || { cat "$target_dir/ios-xcframework.log"; echo "CORE_FAIL xcframework"; exit 1; }
echo "CORE_BUILT $out"
