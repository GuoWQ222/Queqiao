#!/usr/bin/env bash
# The one login form for logging in and signing up (the user, 2026-11-26, as UU Remote), end to end on Linux: a
# queqiao-server of its own on 127.0.0.1 that takes new accounts (--allow-signup), the Linux agent logging in through
# its login form (--test-tray-login: filled and pressed by the program) on Xvfb :98:
#   1. a name the server does not know: the account is made and the agent is online (the form pictured: login.bmp);
#   2. that name with a wrong password: refused, nothing made;
#   3. that name with its password: logged in, nothing made;
#   4. a name given to an account by the server's add-user, with its password: logged in as before.
# Everything the programs keep goes into the test's own folder (XDG_CONFIG_HOME, XDG_STATE_HOME, XDG_RUNTIME_DIR).
# Prints the steps and LOGIN_PASS or LOGIN_FAIL <why>. Needs Xvfb.
# Usage: scripts/run_login_linux.sh (CARGO_TARGET_DIR from the environment).
set -u
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
out=/tmp/queqiao_login
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
# The same features as the other Linux runs (run_takeover_linux.sh), so one build serves them all.
nice -n 10 cargo build -q --release --features queqiao-agent/test-switches,queqiao-viewer/test-switches \
    -p queqiao-agent -p queqiao-viewer -p queqiao-server > "$out/build.log" 2>&1 \
    || { grep -E "^error" -A6 "$out/build.log" | head -30; finish "LOGIN_FAIL build"; }
bin=$CARGO_TARGET_DIR/release
pgrep -f "Xvfb :98 " > /dev/null || { Xvfb :98 -screen 0 1280x720x24 -noreset > "$out/xvfb98.log" 2>&1 & pids+=($!); }
sleep 1

data=$out/server
init=$("$bin/queqiao-server" init --data "$data" 2>&1)
pin=$(printf '%s\n' "$init" | grep -o 'fingerprint: [0-9a-f]\{64\}' | head -1 | cut -d' ' -f2)
[ -n "$pin" ] || finish "LOGIN_FAIL server init"
known=$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')
printf '%s\n' "$known" | "$bin/queqiao-server" add-user alice --data "$data" > "$out/add_user.txt" 2>&1
port=$(( 47600 + RANDOM % 300 ))
"$bin/queqiao-server" serve --listen "127.0.0.1:$port" --data "$data" --public-ip 127.0.0.1 --turn-port 0 \
    --allow-signup > "$out/server.txt" 2>&1 &
pids+=($!)
for _ in $(seq 50); do (exec 3<> "/dev/tcp/127.0.0.1/$port") 2> /dev/null && break; sleep 0.2; done
new_password=$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')

failed=''
step() {
    local name=$1 user=$2 password=$3 report=$out/$1.txt
    shift 3
    DISPLAY=:98 "$bin/queqiao-agent" --test-device "login-$name" --test-tray-login --server "127.0.0.1:$port" \
        --pin "$pin" --user "$user" --password "$password" --seconds 12 --report "$report" "$@" \
        > "$out/$name.out" 2>&1
}
expect() {
    if grep -q -- "$2" "$1" 2> /dev/null; then
        echo "STEP_PASS $3"
    else
        echo "STEP_FAIL $3 (no \"$2\" in $(basename "$1"))"
        failed="$failed $4"
    fi
}
refuse() {
    if grep -q -- "$2" "$1" 2> /dev/null; then
        echo "STEP_FAIL $3 (\"$2\" in $(basename "$1"))"
        failed="$failed $4"
    else
        echo "STEP_PASS $3"
    fi
}

# 1. A new name: the account is made, the agent online.
step new newbie "$new_password" --ui-pictures "$out"
expect "$out/new.txt" "login: a new account newbie was made" "a new name makes the account" new-made
expect "$out/new.txt" "logged in from the tray" "and logs in" new-online
[ -f "$out/agent_login.bmp" ] && echo "STEP_PASS the form pictured" || { echo "STEP_FAIL no picture"; failed="$failed picture"; }

# 2. The same name, a wrong password: refused, nothing made.
step wrong newbie "x$new_password"
refuse "$out/wrong.txt" "logged in from the tray" "a wrong password does not log in" wrong-online
refuse "$out/wrong.txt" "a new account" "a wrong password makes nothing" wrong-made
expect "$out/wrong.txt" "login: " "the form says why" wrong-why

# 3. The same name, its password: logged in, nothing made.
step again newbie "$new_password"
expect "$out/again.txt" "logged in from the tray" "its password logs in" again-online
refuse "$out/again.txt" "a new account" "nothing made the second time" again-made

# 4. An account made by add-user, with its password.
step known alice "$known"
expect "$out/known.txt" "logged in from the tray" "an earlier account logs in" known-online
refuse "$out/known.txt" "a new account" "nothing made for it" known-made

[ -z "$failed" ] && finish LOGIN_PASS
finish "LOGIN_FAIL$failed"
