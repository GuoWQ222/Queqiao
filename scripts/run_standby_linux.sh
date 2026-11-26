#!/usr/bin/env bash
# A standby account server (the user, 2026-11-26: "How would you sync the accounts?"), end to end on Linux on
# 127.0.0.1: an account server of its own, and a relay server joined to it with --standby, which keeps a live copy of
# the accounts:
#   1. the standby joins and gets a full copy; a viewer (a throwaway device, --test-device) logs in and is told of the
#      standby (it keeps it with its login);
#   2. a user added on the account server afterwards (add-user) reaches the copy as a change;
#   3. the account server stops; the standby is made the account server (`serve` on its folder, its certificate the one
#      it named);
#   4. a viewer with the kept list logs in again: the account server cannot be reached, so it connects to the standby,
#      with the old user's password and with the new user's (their Argon2id hashes came with the copy).
# Everything the programs keep goes into the test's own folder (XDG_CONFIG_HOME, XDG_STATE_HOME, XDG_RUNTIME_DIR).
# Prints the steps and STANDBY_PASS or STANDBY_FAIL <why>. Needs Xvfb.
# Usage: scripts/run_standby_linux.sh (CARGO_TARGET_DIR from the environment).
set -u
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
out=/tmp/queqiao_standby
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
nice -n 10 cargo build -q --release --features queqiao-viewer/test-switches -p queqiao-viewer -p queqiao-server \
    > "$out/build.log" 2>&1 || { grep -E "^error" -A6 "$out/build.log" | head -30; finish "STANDBY_FAIL build"; }
bin=$CARGO_TARGET_DIR/release
pgrep -f "Xvfb :96 " > /dev/null || { Xvfb :96 -screen 0 1280x720x24 -noreset > "$out/xvfb96.log" 2>&1 < /dev/null & pids+=($!); }
sleep 1

failed=''
check() {
    if grep -q -- "$2" "$1" 2> /dev/null; then
        echo "STEP_PASS $3"
    else
        echo "STEP_FAIL $3 (no \"$2\" in $(basename "$1"))"
        failed="$failed $3"
    fi
}
wait_for() {
    for _ in $(seq $(( $3 * 5 ))); do grep -q -- "$2" "$1" 2> /dev/null && return 0; sleep 0.2; done
    return 1
}
secret() { head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n'; }

data=$out/server
init=$("$bin/queqiao-server" init --data "$data" 2>&1)
pin=$(printf '%s\n' "$init" | grep -o 'fingerprint: [0-9a-f]\{64\}' | head -1 | cut -d' ' -f2)
[ -n "$pin" ] || finish "STANDBY_FAIL server init"
alice=$(secret); bob=$(secret)
printf '%s\n' "$alice" | "$bin/queqiao-server" add-user alice --data "$data" > "$out/add_alice.txt" 2>&1
base=$(( 49000 + RANDOM % 900 ))
port=$base; standby_port=$(( base + 1 )); turn=$(( base + 2 ))
"$bin/queqiao-server" serve --listen "127.0.0.1:$port" --data "$data" --public-ip 127.0.0.1 --turn-port 0 \
    > "$out/server.txt" 2>&1 < /dev/null &
server=$!
pids+=($server)
for _ in $(seq 50); do (exec 3<> "/dev/tcp/127.0.0.1/$port") 2> /dev/null && break; sleep 0.2; done

# 1. The standby joins with a full copy; a login is told of it.
"$bin/queqiao-server" add-relay s1 --data "$data" 2> /dev/null \
    | "$bin/queqiao-server" relay-key --data "$out/s1" > "$out/s1_key.txt" 2>&1
"$bin/queqiao-server" relay --central "127.0.0.1:$port" --pin "$pin" --public-ip 127.0.0.1 --turn-port "$turn" \
    --relay-ports "$(( base + 10 ))-$(( base + 29 ))" --relay-private --standby --accounts-port "$standby_port" \
    --data "$out/s1" > "$out/s1.txt" 2>&1 < /dev/null &
relay=$!
pids+=($relay)
wait_for "$out/s1.txt" "standby: the full copy is done" 20
check "$out/s1.txt" "standby: the full copy is done" "the standby got a full copy"
standby_pin=$(grep -o 'certificate [0-9a-f]\{64\}' "$out/s1.txt" | head -1 | cut -d' ' -f2)
sleep 3
list() {
    # shellcheck disable=SC2086
    DISPLAY=:96 "$bin/queqiao-viewer" --server "127.0.0.1:$port" --pin "$pin" --user "$1" --password "$2" \
        --test-device "lister" --list --report "$out/$3.txt" > "$out/$3.out" 2>&1 < /dev/null
}
list alice "$alice" list1
check "$out/list1.txt" "RESULT PASS" "alice logs in to the account server"
check "$XDG_CONFIG_HOME/queqiao/standbys_test.json" "127.0.0.1:$standby_port" "the login keeps the standby"
check "$XDG_CONFIG_HOME/queqiao/standbys_test.json" "$standby_pin" "with the certificate it named"

# 2. A user added afterwards reaches the copy.
printf '%s\n' "$bob" | "$bin/queqiao-server" add-user bob --data "$data" > "$out/add_bob.txt" 2>&1
sleep 4

# 3. The account server stops; the standby becomes the account server on its copy.
kill "$server"; wait "$server" 2> /dev/null
kill "$relay"; wait "$relay" 2> /dev/null
"$bin/queqiao-server" serve --listen "127.0.0.1:$standby_port" --data "$out/s1" --public-ip 127.0.0.1 --turn-port 0 \
    > "$out/promoted.txt" 2>&1 < /dev/null &
pids+=($!)
for _ in $(seq 50); do (exec 3<> "/dev/tcp/127.0.0.1/$standby_port") 2> /dev/null && break; sleep 0.2; done
check "$out/promoted.txt" "$standby_pin" "the promoted standby serves with the certificate it named"

# 4. Logins fall back to it: the old user's password and the new user's.
list alice "$alice" list2
check "$out/list2.txt" "RESULT PASS" "alice logs in after the account server stopped"
check "$out/list2.txt" "connected to its standby 127.0.0.1:$standby_port" "through the standby"
list bob "$bob" list3
check "$out/list3.txt" "RESULT PASS" "bob, added after the full copy, logs in on the standby"
list bob "wrong password" list4
if grep -q "RESULT PASS" "$out/list4.txt" 2> /dev/null; then
    echo "STEP_FAIL a wrong password is refused on the standby"; failed="$failed wrong-password"
else
    echo "STEP_PASS a wrong password is refused on the standby"
fi

if [ -n "$failed" ]; then
    finish "STANDBY_FAIL$failed"
fi
finish STANDBY_PASS
