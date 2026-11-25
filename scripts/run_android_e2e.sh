#!/usr/bin/env bash
# The Android app end to end, in an emulator that is already running (adb sees it): a queqiao-server of its own on
# 127.0.0.1 (the emulator reaches it as 10.0.2.2), the Linux agent on Xvfb :91 logged in as the test's user, the app
# and its test installed, then SessionTest (mobile/android/app/src/androidTest): log in, the PC listed, connect, at
# least 30 frames decoded and shown, a tap, a swipe and typing sent, disconnect. Checks the agent's report too: a
# session, frames sent, input events received. The pictures the test took and every log go to OUT (default
# /tmp/android_e2e).
# Needs: the APKs built (scripts/build_android.sh debug, then gradle assembleDebugAndroidTest), queqiao-server and
# queqiao-agent built (cargo build --release), Xvfb and xclock, adb.
# Prints ANDROID_E2E_PASS or ANDROID_E2E_FAIL <why>.
set -u
cd "$(dirname "$0")/.." || exit 1
out=${OUT:-/tmp/android_e2e}
rm -rf "$out"; mkdir -p "$out"
bin=${CARGO_TARGET_DIR:-$PWD/target}/release
apk=mobile/android/app/build/outputs/apk/debug/app-debug.apk
test_apk=mobile/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
export XDG_CONFIG_HOME=$out/config XDG_STATE_HOME=$out/state XDG_RUNTIME_DIR=$out/run
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"; mkdir -m 700 "$XDG_RUNTIME_DIR"
pids=()
finish() {
    kill "${pids[@]}" 2> /dev/null
    echo "$1"
    exit 0
}
for file in "$apk" "$test_apk" "$bin/queqiao-server" "$bin/queqiao-agent"; do
    [ -f "$file" ] || finish "ANDROID_E2E_FAIL missing $file"
done

Xvfb :91 -screen 0 1280x720x24 -noreset > "$out/xvfb.log" 2>&1 &
pids+=($!)
sleep 1
DISPLAY=:91 xclock -update 1 -geometry 400x400+100+100 > /dev/null 2>&1 &
pids+=($!)

data=$out/server
init=$("$bin/queqiao-server" init --data "$data" 2>&1)
pin=$(printf '%s\n' "$init" | grep -o 'fingerprint: [0-9a-f]\{64\}' | head -1 | cut -d' ' -f2)
[ -n "$pin" ] || { echo "$init"; finish "ANDROID_E2E_FAIL server init"; }
password=$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')
printf '%s\n' "$password" | "$bin/queqiao-server" add-user tester --data "$data" > "$out/add_user.txt" 2>&1
port=$(( 47600 + RANDOM % 300 ))
"$bin/queqiao-server" serve --listen "127.0.0.1:$port" --data "$data" --public-ip 127.0.0.1 --turn-port 0 \
    > "$out/server.txt" 2>&1 &
pids+=($!)
for _ in $(seq 50); do (exec 3<> "/dev/tcp/127.0.0.1/$port") 2> /dev/null && break; sleep 0.2; done

device=$(cat /proc/sys/kernel/hostname)
DISPLAY=:91 "$bin/queqiao-agent" --server "127.0.0.1:$port" --pin "$pin" --user tester --password "$password" \
    --sessions 1 --seconds 600 --report "$out/agent_report.txt" > "$out/agent.txt" 2>&1 &
agent=$!
pids+=($agent)
sleep 3

adb install -r -g "$apk" > "$out/install.txt" 2>&1 || { cat "$out/install.txt"; finish "ANDROID_E2E_FAIL install app"; }
adb install -r -g "$test_apk" >> "$out/install.txt" 2>&1 || { cat "$out/install.txt"; finish "ANDROID_E2E_FAIL install test"; }
adb logcat -c
adb logcat > "$out/logcat.txt" 2>&1 &
pids+=($!)
adb shell am instrument -w -e server "10.0.2.2:$port" -e pin "$pin" -e user tester -e password "$password" \
    -e device "$device" com.queqiao.app.test/androidx.test.runner.AndroidJUnitRunner > "$out/instrument.txt" 2>&1
for name in login devices session toolbar after; do
    adb exec-out run-as com.queqiao.app cat "files/test_pictures/$name.png" > "$out/$name.png" 2> /dev/null
    [ -s "$out/$name.png" ] || rm -f "$out/$name.png"
done
# The agent's session has ended (the app disconnected): wait for its report.
for _ in $(seq 40); do [ -s "$out/agent_report.txt" ] && break; sleep 0.5; done
kill -INT "$agent" 2> /dev/null
for _ in $(seq 20); do [ -s "$out/agent_report.txt" ] && break; sleep 0.5; done
cat "$out/instrument.txt" | tail -15
cat "$out/agent_report.txt" 2> /dev/null
grep -q "^OK (1 test)" "$out/instrument.txt" || finish "ANDROID_E2E_FAIL app test"
frames=$(grep -o '[0-9]* frames sent' "$out/agent_report.txt" | head -1 | cut -d' ' -f1)
inputs=$(grep -o 'input: [0-9]* events' "$out/agent_report.txt" | head -1 | cut -d' ' -f2)
[ "${frames:-0}" -gt 0 ] || finish "ANDROID_E2E_FAIL the agent sent no frames"
[ "${inputs:-0}" -gt 0 ] || finish "ANDROID_E2E_FAIL the agent received no input"
finish ANDROID_E2E_PASS
