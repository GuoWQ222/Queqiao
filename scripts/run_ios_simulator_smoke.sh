#!/bin/bash
# The iOS app in the iPhone Simulator (on a Mac, after the Simulator build of .github/workflows/ios.yml): a
# queqiao-server of its own on 127.0.0.1 (the Simulator shares the Mac's network) with one user, the app installed and
# started: a picture of its login form; started again with a test login (QUEQIAO_TEST_LOGIN): the app's device must
# appear in the server's database, and a picture of its device list; then a session with a stand-in PC
# (examples/test_agent): its picture decoded and pictured, typed text received. Pictures and logs go to OUT (default
# /tmp/ios_results/smoke).
# Needs: queqiao-server and the test agent built (cargo build --release -p queqiao-server; cargo build --release
# -p queqiao-mobile --example test_agent), the app at APP (default
# build/sim/Build/Products/Debug-iphonesimulator/Queqiao.app), Xcode's simctl.
# Prints IOS_SMOKE_PASS or IOS_SMOKE_FAIL <why>.
set -u
cd "$(dirname "$0")/.." || exit 1
out=${OUT:-/tmp/ios_results/smoke}
app=${APP:-build/sim/Build/Products/Debug-iphonesimulator/Queqiao.app}
bin=${CARGO_TARGET_DIR:-target}/release
rm -rf "$out"; mkdir -p "$out"
pids=()
finish() { kill "${pids[@]}" 2> /dev/null; echo "$1"; exit 0; }
[ -d "$app" ] || finish "IOS_SMOKE_FAIL no app at $app"
[ -x "$bin/queqiao-server" ] || finish "IOS_SMOKE_FAIL no queqiao-server"

data=$out/server
init=$("$bin/queqiao-server" init --data "$data" 2>&1)
pin=$(printf '%s\n' "$init" | grep -o 'fingerprint: [0-9a-f]\{64\}' | head -1 | cut -d' ' -f2)
[ -n "$pin" ] || { echo "$init"; finish "IOS_SMOKE_FAIL server init"; }
password=$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')
printf '%s\n' "$password" | "$bin/queqiao-server" add-user tester --data "$data" > "$out/add_user.txt" 2>&1
port=$(( 47600 + RANDOM % 300 ))
"$bin/queqiao-server" serve --listen "127.0.0.1:$port" --data "$data" --public-ip 127.0.0.1 --turn-port 0 \
    > "$out/server.txt" 2>&1 &
pids+=($!)
for _ in $(seq 50); do nc -z 127.0.0.1 "$port" 2> /dev/null && break; sleep 0.2; done

# The first iPhone the Simulator has.
device=$(xcrun simctl list devices available | grep -m1 -oE 'iPhone [^(]*\(([0-9A-F-]{36})\)' | grep -oE '[0-9A-F-]{36}')
[ -n "$device" ] || finish "IOS_SMOKE_FAIL no iPhone in the Simulator"
xcrun simctl boot "$device" > "$out/boot.txt" 2>&1
xcrun simctl bootstatus "$device" -b > /dev/null 2>&1
xcrun simctl install "$device" "$app" > "$out/install.txt" 2>&1 || { cat "$out/install.txt"; finish "IOS_SMOKE_FAIL install"; }
xcrun simctl launch "$device" com.queqiao.app > "$out/launch1.txt" 2>&1 || { cat "$out/launch1.txt"; finish "IOS_SMOKE_FAIL launch"; }
sleep 8
xcrun simctl io "$device" screenshot "$out/login.png" > /dev/null 2>&1
xcrun simctl terminate "$device" com.queqiao.app > /dev/null 2>&1
# Started again with a test login: the device list (the app's device appears in the server's database).
SIMCTL_CHILD_QUEQIAO_TEST_LOGIN="127.0.0.1:$port|$pin|tester|$password" \
    xcrun simctl launch "$device" com.queqiao.app > "$out/launch2.txt" 2>&1 || finish "IOS_SMOKE_FAIL launch 2"
app_devices() { sqlite3 "$data/queqiao.db" "select name from devices" 2> /dev/null; }
for _ in $(seq 40); do [ -n "$(app_devices)" ] && break; sleep 0.5; done
sleep 4
xcrun simctl io "$device" screenshot "$out/devices.png" > /dev/null 2>&1
echo "devices on the server: $(app_devices | paste -sd, -)"
[ -n "$(app_devices)" ] || finish "IOS_SMOKE_FAIL the app did not log in (no device on the server)"
xcrun simctl terminate "$device" com.queqiao.app > /dev/null 2>&1

# A session: the stand-in PC (examples/test_agent: a moving test picture, it counts what it receives) on the account;
# the app (its saved login) connects to it by name and, once the picture shows, types a text (QUEQIAO_TEST_CONNECT,
# QUEQIAO_TEST_TYPE). The agent ends the session after 40 s and writes what it got.
typed=$(printf 'typed \344\270\255\346\226\207 \303\251')
"$bin/examples/test_agent" "$out/agent" "127.0.0.1:$port" "$pin" tester "$password" sim-test-pc 40 \
    "$out/agent_report.txt" > "$out/agent.txt" 2>&1 &
pids+=($!)
sleep 3
SIMCTL_CHILD_QUEQIAO_TEST_CONNECT=sim-test-pc SIMCTL_CHILD_QUEQIAO_TEST_TYPE="$typed" \
    xcrun simctl launch "$device" com.queqiao.app > "$out/launch3.txt" 2>&1 || finish "IOS_SMOKE_FAIL launch 3"
sleep 15
xcrun simctl io "$device" screenshot "$out/session.png" > /dev/null 2>&1
for _ in $(seq 80); do [ -s "$out/agent_report.txt" ] && break; sleep 0.5; done
sleep 2
xcrun simctl io "$device" screenshot "$out/after.png" > /dev/null 2>&1
xcrun simctl spawn "$device" log show --last 3m --predicate 'process == "Queqiao"' > "$out/app_log.txt" 2>&1
xcrun simctl shutdown "$device" > /dev/null 2>&1
cat "$out/agent_report.txt" 2> /dev/null
grep -q "^session: yes" "$out/agent_report.txt" 2> /dev/null || finish "IOS_SMOKE_FAIL no session with the test agent"
frames=$(grep -o '^frames sent: [0-9]*' "$out/agent_report.txt" | grep -o '[0-9]*$')
[ "${frames:-0}" -ge 100 ] || finish "IOS_SMOKE_FAIL the test agent sent ${frames:-no} frames"
grep -qF "typed: $typed" "$out/agent_report.txt" || finish "IOS_SMOKE_FAIL the typed text did not arrive"
finish IOS_SMOKE_PASS
