#!/usr/bin/env bash
# Several relays (the user, 2026-11-26: spread the relayed traffic over the servers), end to end on Linux: an account
# server of its own on 127.0.0.1 without a relay of its own (--turn-port 0) and two relay servers joined to it
# (add-relay, relay-key, relay), the Linux agent on Xvfb :95 and viewers (throwaway devices of the same account,
# --test-device) on :96, every session forced through a relay (--force network:relay):
#   1. both relays join; a relay with a wrong key, and one with a name the server does not know, are refused;
#   2. a session: both ends get r1 first (equally near, by name) and are relayed through it, the window naming it;
#   3. r1 frozen (SIGSTOP: its link stays open, so for a few seconds it is still handed out): the next session's ends
#      ask r1, get no answer, ask r2 too and are relayed through r2;
#   4. r1 running again but taken out (remove-relay): the server drops it at its next report, and the next session
#      goes through r2 without asking r1.
# Everything the programs keep goes into the test's own folder (XDG_CONFIG_HOME, XDG_STATE_HOME, XDG_RUNTIME_DIR).
# Prints the steps and RELAYS_PASS or RELAYS_FAIL <why>. Needs Xvfb, xclock.
# Usage: scripts/run_relays_linux.sh (CARGO_TARGET_DIR from the environment).
set -u
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
out=/tmp/queqiao_relays
rm -rf "$out"; mkdir -p "$out"
export XDG_CONFIG_HOME=$out/config XDG_STATE_HOME=$out/state XDG_RUNTIME_DIR=$out/run
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
mkdir -m 700 "$XDG_RUNTIME_DIR"
pids=()
finish() {
    kill -CONT "${pids[@]}" 2> /dev/null
    kill "${pids[@]}" 2> /dev/null
    echo "$1"
    exit 0
}
nice -n 10 cargo build -q --release --features queqiao-agent/test-switches,queqiao-viewer/test-switches \
    -p queqiao-agent -p queqiao-viewer -p queqiao-server > "$out/build.log" 2>&1 \
    || { grep -E "^error" -A6 "$out/build.log" | head -30; finish "RELAYS_FAIL build"; }
bin=$CARGO_TARGET_DIR/release
for display in 95 96; do
    pgrep -f "Xvfb :$display " > /dev/null || { Xvfb ":$display" -screen 0 1280x720x24 -noreset > "$out/xvfb$display.log" 2>&1 < /dev/null & pids+=($!); }
done
sleep 1
DISPLAY=:95 xclock -update 1 -geometry 300x300+100+100 > /dev/null 2>&1 < /dev/null &
pids+=($!)

failed=''
check() {
    if grep -q -- "$2" "$1" 2> /dev/null; then
        echo "STEP_PASS $3"
    else
        echo "STEP_FAIL $3 (no \"$2\" in $(basename "$1"))"
        failed="$failed $3"
    fi
}
absent() {
    if grep -q -- "$2" "$1" 2> /dev/null; then
        echo "STEP_FAIL $3 (\"$2\" in $(basename "$1"))"
        failed="$failed $3"
    else
        echo "STEP_PASS $3"
    fi
}
# Waits up to $3 seconds for "$2" in file $1.
wait_for() {
    for _ in $(seq $(( $3 * 5 ))); do grep -q -- "$2" "$1" 2> /dev/null && return 0; sleep 0.2; done
    return 1
}

data=$out/server
init=$("$bin/queqiao-server" init --data "$data" 2>&1)
pin=$(printf '%s\n' "$init" | grep -o 'fingerprint: [0-9a-f]\{64\}' | head -1 | cut -d' ' -f2)
[ -n "$pin" ] || finish "RELAYS_FAIL server init"
password=$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')
printf '%s\n' "$password" | "$bin/queqiao-server" add-user alice --data "$data" > "$out/add_user.txt" 2>&1
base=$(( 48000 + RANDOM % 1000 ))
port=$(( base + 50 ))
"$bin/queqiao-server" serve --listen "127.0.0.1:$port" --data "$data" --public-ip 127.0.0.1 --turn-port 0 \
    > "$out/server.txt" 2>&1 < /dev/null &
pids+=($!)
for _ in $(seq 50); do (exec 3<> "/dev/tcp/127.0.0.1/$port") 2> /dev/null && break; sleep 0.2; done

# Relay servers: a key from the account server, kept with relay-key, then STUN/TURN joined to it.
relay() {
    local name=$1 first=$2 turn=$3
    "$bin/queqiao-server" relay --central "127.0.0.1:$port" --pin "$pin" --public-ip 127.0.0.1 --turn-port "$turn" \
        --relay-ports "$first-$(( first + 19 ))" --relay-private --data "$out/$name" > "$out/$name.txt" 2>&1 < /dev/null &
    pids+=($!)
    eval "pid_$name=$!"
}
for name in r1 r2; do
    "$bin/queqiao-server" add-relay "$name" --data "$data" 2> /dev/null \
        | "$bin/queqiao-server" relay-key --data "$out/$name" > "$out/${name}_key.txt" 2>&1
done
turn1=$(( base + 40 )); turn2=$(( base + 41 ))
relay r1 "$base" "$turn1"
relay r2 "$(( base + 20 ))" "$turn2"
# 1. Both join; a wrong key and an unknown name are refused.
wait_for "$out/server.txt" "relay r1 joined" 15; wait_for "$out/server.txt" "relay r2 joined" 15
check "$out/server.txt" "relay r1 joined" "relay r1 joined"
check "$out/server.txt" "relay r2 joined" "relay r2 joined"
mkdir -p "$out/wrong" "$out/stranger"
printf 'r1:%s\n' "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')" | "$bin/queqiao-server" relay-key --data "$out/wrong" > /dev/null 2>&1
printf 'r9:%s\n' "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')" | "$bin/queqiao-server" relay-key --data "$out/stranger" > /dev/null 2>&1
relay wrong "$(( base + 60 ))" "$(( base + 42 ))"
relay stranger "$(( base + 80 ))" "$(( base + 43 ))"
wait_for "$out/wrong.txt" "refused" 10; wait_for "$out/stranger.txt" "refused" 10
check "$out/wrong.txt" "refused: not a relay of this server, or not its key" "a wrong key is refused"
check "$out/stranger.txt" "refused: not a relay of this server, or not its key" "an unknown relay is refused"
kill "$pid_wrong" "$pid_stranger" 2> /dev/null
# Their reports carry where they are: give them a few.
sleep 3

account="--server 127.0.0.1:$port --pin $pin --user alice --password $password"
device=$(cat /proc/sys/kernel/hostname)
# shellcheck disable=SC2086
DISPLAY=:95 "$bin/queqiao-agent" $account --sessions 3 --seconds 240 --force network:relay --report "$out/agent.txt" \
    > "$out/agent.out" 2>&1 < /dev/null &
agent=$!
pids+=($agent)
sleep 3
session() {
    # shellcheck disable=SC2086
    DISPLAY=:96 "$bin/queqiao-viewer" $account --test-device "viewer$1" --device "$device" --seconds 12 --width 640 \
        --force network:relay --report "$out/viewer$1.txt" "${@:2}" > "$out/viewer$1.out" 2>&1 < /dev/null
}
mkdir -p "$out/pictures"

# 2. Through r1, both ends.
session 1 --ui-pictures "$out/pictures"
check "$out/viewer1.txt" "relayed through 127.0.0.1:$turn1" "session 1: the viewer relayed through r1"
# The relay's name comes with it (the session window's card and title show its place).
check "$out/viewer1.txt" "relayed through 127.0.0.1:$turn1 (r1) to " "session 1: the relay's name in the path"
check "$out/viewer1.txt" "| relayed (R1)" "session 1: the window title names the relay"

# 3. r1 frozen: both ends fall back to r2 while gathering (the agent's report, written when it ends, is checked last).
kill -STOP "$pid_r1"
session 2
check "$out/viewer2.txt" "asking 127.0.0.1:$turn2 too" "session 2: the viewer asked r2 when r1 did not answer"
check "$out/viewer2.txt" "relayed through 127.0.0.1:$turn2" "session 2: the viewer relayed through r2"
kill -CONT "$pid_r1"

# 4. r1 taken out: dropped at its next report, not asked any more.
"$bin/queqiao-server" remove-relay r1 --data "$data" > "$out/remove.txt" 2>&1
wait_for "$out/server.txt" "relay r1 left" 15
check "$out/server.txt" "relay r1 left" "a relay taken out is dropped"
check "$out/r1.txt" "this relay was taken out of the server" "the relay taken out is told"
session 3
check "$out/viewer3.txt" "relayed through 127.0.0.1:$turn2" "session 3: through r2"
absent "$out/viewer3.txt" "asking 127.0.0.1:$turn2 too" "session 3: r1 is not asked"
# The agent ends after its three sessions: the same relays at its end (r1 only in the first session).
for _ in $(seq 60); do kill -0 "$agent" 2> /dev/null || break; sleep 0.5; done
check "$out/agent.txt" "relayed through 127.0.0.1:$turn1" "the agent relayed session 1 through r1"
check "$out/agent.txt" "asking 127.0.0.1:$turn2 too" "the agent asked r2 when r1 did not answer"
[ "$(grep -c "relayed through 127.0.0.1:$turn2" "$out/agent.txt")" -ge 2 ] \
    && echo "STEP_PASS the agent relayed sessions 2 and 3 through r2" \
    || { echo "STEP_FAIL the agent relayed sessions 2 and 3 through r2"; failed="$failed agent-r2"; }

if [ -n "$failed" ]; then
    finish "RELAYS_FAIL$failed"
fi
finish RELAYS_PASS
