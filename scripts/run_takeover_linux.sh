#!/usr/bin/env bash
# One device controls a PC at a time (the user, 2026-11-26, as UU Remote), end to end on Linux: a queqiao-server of its
# own on 127.0.0.1, the Linux agent on Xvfb :95, viewers (throwaway devices of the same account, --test-device) on
# Xvfb :96:
#   1. viewer A connects; the account's device list says the PC is controlled by viewerA;
#   2. viewer B asks without taking over: refused, told viewerA controls it;
#   3. viewer B asks taking over (--take-over, the main window's Take Control): A's session ends with the agent's word
#      that viewerB took over; the list says viewerB;
#   4. a third device ends B's session (--end-control, the banner's Disconnect): B is told; the list says nobody.
# Everything the programs keep goes into the test's own folder (XDG_CONFIG_HOME, XDG_STATE_HOME, XDG_RUNTIME_DIR).
# Prints the steps and TAKEOVER_PASS or TAKEOVER_FAIL <why>. Needs Xvfb, xclock.
# Usage: scripts/run_takeover_linux.sh (CARGO_TARGET_DIR from the environment).
set -u
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
out=/tmp/queqiao_takeover
rm -rf "$out"; mkdir -p "$out"
export XDG_CONFIG_HOME=$out/config XDG_STATE_HOME=$out/state XDG_RUNTIME_DIR=$out/run
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
mkdir -m 700 "$XDG_RUNTIME_DIR"
pids=()
finish() {
    kill "${pids[@]}" 2> /dev/null
    echo "$1"
    exit 0
}
nice -n 10 cargo build -q --release --features queqiao-agent/test-switches,queqiao-viewer/test-switches \
    -p queqiao-agent -p queqiao-viewer -p queqiao-server > "$out/build.log" 2>&1 \
    || { grep -E "^error" -A6 "$out/build.log" | head -30; finish "TAKEOVER_FAIL build"; }
bin=$CARGO_TARGET_DIR/release
for display in 95 96; do
    pgrep -f "Xvfb :$display " > /dev/null || { Xvfb ":$display" -screen 0 1280x720x24 -noreset > "$out/xvfb$display.log" 2>&1 & pids+=($!); }
done
sleep 1
DISPLAY=:95 xclock -update 1 -geometry 300x300+100+100 > /dev/null 2>&1 &
pids+=($!)

data=$out/server
init=$("$bin/queqiao-server" init --data "$data" 2>&1)
pin=$(printf '%s\n' "$init" | grep -o 'fingerprint: [0-9a-f]\{64\}' | head -1 | cut -d' ' -f2)
[ -n "$pin" ] || finish "TAKEOVER_FAIL server init"
password=$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')
printf '%s\n' "$password" | "$bin/queqiao-server" add-user alice --data "$data" > "$out/add_user.txt" 2>&1
port=$(( 47600 + RANDOM % 300 ))
"$bin/queqiao-server" serve --listen "127.0.0.1:$port" --data "$data" --public-ip 127.0.0.1 --turn-port 0 \
    > "$out/server.txt" 2>&1 &
pids+=($!)
for _ in $(seq 50); do (exec 3<> "/dev/tcp/127.0.0.1/$port") 2> /dev/null && break; sleep 0.2; done
account="--server 127.0.0.1:$port --pin $pin --user alice --password $password"
device=$(cat /proc/sys/kernel/hostname)

# shellcheck disable=SC2086
DISPLAY=:95 "$bin/queqiao-agent" $account --sessions 3 --seconds 180 --report "$out/agent.txt" > "$out/agent.out" 2>&1 &
agent=$!
pids+=($agent)
sleep 3
viewer() {
    local name=$1; shift
    # shellcheck disable=SC2086
    DISPLAY=:96 "$bin/queqiao-viewer" $account --test-device "$name" "$@"
}
listed() {
    viewer lister --list --report "$out/list_$1.txt" > /dev/null 2>&1
    grep -o "device $device ([^)]*): [^,]*\(, controlled by [^ ]*\)\?" "$out/list_$1.txt" | head -1
}
failed=''
check() {
    if grep -q -- "$2" "$1" 2> /dev/null; then
        echo "STEP_PASS $3"
    else
        echo "STEP_FAIL $3 (no \"$2\" in $(basename "$1"))"
        failed="$failed $3"
    fi
}

# 1. Viewer A controls the PC.
viewer viewerA --device "$device" --seconds 60 --width 640 --report "$out/viewer_a.txt" > "$out/viewer_a.out" 2>&1 &
viewer_a=$!
pids+=($viewer_a)
sleep 10
echo "list: $(listed 1)"
check "$out/list_1.txt" "controlled by viewerA" "the list says viewerA controls it"
# Another device's main window meanwhile (an agent of its own, panelpc, on Xvfb :97): its Devices page shows that PC
# (the first other device) with the "being controlled" banner, and the list its chip: main_devices.bmp.
pgrep -f "Xvfb :97 " > /dev/null || { Xvfb :97 -screen 0 1280x720x24 -noreset > "$out/xvfb97.log" 2>&1 & pids+=($!); }
mkdir -p "$out/panel" "$out/config2" && mkdir -m 700 "$out/run2"
# shellcheck disable=SC2086
XDG_CONFIG_HOME=$out/config2 XDG_RUNTIME_DIR=$out/run2 DISPLAY=:97 "$bin/queqiao-agent" $account --test-device panelpc \
    --test-main-window --ui-pictures "$out/panel" --seconds 9 --report "$out/agent_panel.txt" > "$out/agent_panel.out" 2>&1
check "$out/agent_panel.txt" "main window: pictures of its pages saved" "another device's main window pictured"

# 2. Viewer B without taking over: told it is busy, with whom.
viewer viewerB --device "$device" --seconds 5 --width 640 --report "$out/viewer_b1.txt" > "$out/viewer_b1.out" 2>&1
check "$out/viewer_b1.txt" "is busy: viewerA controls it" "B without taking over is told viewerA controls it"

# 3. Viewer B takes control: A's session ends with the agent's word.
viewer viewerB --device "$device" --take-over --seconds 60 --width 640 --report "$out/viewer_b2.txt" > "$out/viewer_b2.out" 2>&1 &
viewer_b=$!
pids+=($viewer_b)
for _ in $(seq 100); do kill -0 "$viewer_a" 2> /dev/null || break; sleep 0.2; done
wait "$viewer_a" 2> /dev/null
check "$out/viewer_a.txt" "viewerB, a device of your account, took over control of this PC" "A is told viewerB took over"
sleep 8
echo "list: $(listed 2)"
check "$out/list_2.txt" "controlled by viewerB" "the list says viewerB controls it"

# 4. Another device ends B's session (the banner's Disconnect).
viewer ender --end-control "$device" --report "$out/end.txt" > /dev/null 2>&1
for _ in $(seq 100); do kill -0 "$viewer_b" 2> /dev/null || break; sleep 0.2; done
wait "$viewer_b" 2> /dev/null
check "$out/viewer_b2.txt" "ender, a device of your account, ended this remote control session" "B is told ender ended it"
sleep 3
# The agent is still online (a third session is to come): nobody is named any more.
echo "list: $(listed 3)"
if grep -q "controlled by" "$out/list_3.txt" || ! grep -q "$device ([^)]*): online" "$out/list_3.txt"; then
    echo "STEP_FAIL the list does not say online and free"; failed="$failed list-3"
else
    echo "STEP_PASS the list names nobody once it ended (the PC online)"
fi

# 5. A new session as ever, nobody to take over from (the agent's third: it then leaves).
viewer viewerC --device "$device" --seconds 5 --width 640 --report "$out/viewer_c.txt" > "$out/viewer_c.out" 2>&1
check "$out/viewer_c.txt" "RESULT PASS" "C connects as ever afterwards"

# The agent: its two sessions and what it did.
for _ in $(seq 100); do kill -0 "$agent" 2> /dev/null || break; sleep 0.2; done
kill "$agent" 2> /dev/null
wait "$agent" 2> /dev/null
check "$out/agent.txt" "told this PC is busy" "the agent told B it was busy"
check "$out/agent.txt" "viewerB took over control of this PC" "the agent let B take over"
check "$out/agent.txt" "ender ended this session from its device list" "the agent ended B's session for ender"
[ -z "$failed" ] && finish TAKEOVER_PASS
finish "TAKEOVER_FAIL$failed"
