#!/bin/bash
# Builds the Android app: the core for arm64-v8a and x86_64 (scripts/build_mobile_core.sh, into the app's
# jniLibs), then the APK with Gradle (mobile/android). Needs ANDROID_HOME (the SDK), a JDK 17, the NDK (NDK_HOME)
# and Gradle 8.10 (GRADLE, default gradle on the PATH).
# Usage: scripts/build_android.sh [debug|release] (default debug). Prints APK_BUILT <apk> or APK_FAIL <step>.
set -u
cd "$(dirname "$0")/.." || exit 1
kind=${1:-debug}
libs=mobile/android/app/src/main/jniLibs
core=$(bash scripts/build_mobile_core.sh "$libs")
if ! echo "$core" | tail -1 | grep -q CORE_BUILT; then
    echo "$core" | tail -30
    echo "APK_FAIL core"
    exit 1
fi
task=assemble$(echo "${kind:0:1}" | tr a-z A-Z)${kind:1}
log=${CARGO_TARGET_DIR:-target}/android-gradle.log
if ! (cd mobile/android && ${GRADLE:-gradle} -q "$task") > "$log" 2>&1; then
    grep -E "^e: |error:|What went wrong" -A 3 "$log" | head -40
    echo "APK_FAIL gradle (log: $log)"
    exit 1
fi
apk=$(ls mobile/android/app/build/outputs/apk/"$kind"/*.apk | head -1)
echo "APK_BUILT $apk"
