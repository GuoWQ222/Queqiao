#!/usr/bin/env bash
# The mobile core (crates/queqiao-mobile) against a queqiao-server of its own on 127.0.0.1 and the Linux agent on
# Xvfb :93, without a phone: examples/core_probe logs in as the apps do, connects, counts frames, sends input. Checks
# the agent's report (frames sent, input received) too. Output in /tmp/queqiao_core_probe.
# Prints CORE_PROBE_PASS ... and MOBILE_CORE_PASS, or MOBILE_CORE_FAIL <why>.
set -u
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
out=/tmp/queqiao_core_probe
rm -rf "$out"; mkdir -p "$out"
export XDG_CONFIG_HOME=$out/config XDG_STATE_HOME=$out/state XDG_RUNTIME_DIR=$out/run
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"; mkdir -m 700 "$XDG_RUNTIME_DIR"
nice -n 10 cargo build -q --release -p queqiao-server -p queqiao-agent -p queqiao-mobile --example core_probe \
    > "$out/build.log" 2>&1 || { grep -E "^error" -A6 "$out/build.log" | head -30; echo MOBILE_CORE_FAIL build; exit 0; }
bin=$CARGO_TARGET_DIR/release
pids=()
finish() { kill "${pids[@]}" 2> /dev/null; echo "$1"; exit 0; }
pgrep -f "Xvfb :93 " > /dev/null || { Xvfb :93 -screen 0 1280x720x24 -noreset > "$out/xvfb.log" 2>&1 & pids+=($!); }
sleep 1
DISPLAY=:93 xclock -update 1 -geometry 300x300+100+100 > /dev/null 2>&1 &
pids+=($!)
data=$out/server
init=$("$bin/queqiao-server" init --data "$data" 2>&1)
pin=$(printf '%s\n' "$init" | grep -o 'fingerprint: [0-9a-f]\{64\}' | head -1 | cut -d' ' -f2)
[ -n "$pin" ] || finish "MOBILE_CORE_FAIL server init"
password=$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')
printf '%s\n' "$password" | "$bin/queqiao-server" add-user tester --data "$data" > /dev/null 2>&1
port=$(( 47600 + RANDOM % 300 ))
"$bin/queqiao-server" serve --listen "127.0.0.1:$port" --data "$data" --public-ip 127.0.0.1 --turn-port 0 \
    > "$out/server.txt" 2>&1 &
pids+=($!)
for _ in $(seq 50); do (exec 3<> "/dev/tcp/127.0.0.1/$port") 2> /dev/null && break; sleep 0.2; done
device=$(cat /proc/sys/kernel/hostname)
DISPLAY=:93 "$bin/queqiao-agent" --server "127.0.0.1:$port" --pin "$pin" --user tester --password "$password" \
    --sessions 1 --seconds 90 --report "$out/agent_report.txt" > "$out/agent.txt" 2>&1 &
agent=$!
pids+=($agent)
sleep 3
# The probe's own folder: its device key apart from the agent's.
probe_folder=$out/probe
mkdir -p "$probe_folder"
"$bin/examples/core_probe" "$probe_folder" "127.0.0.1:$port" "$pin" tester "$password" "$device" "${SECONDS_RUN:-10}" \
    > "$out/probe.txt" 2>&1
for _ in $(seq 40); do [ -s "$out/agent_report.txt" ] && break; sleep 0.5; done
grep -v '^event: ' "$out/probe.txt" | tail -5
grep '^event: ' "$out/probe.txt" | cut -c1-160 | head -12
cat "$out/agent_report.txt" 2> /dev/null | head -20
grep -q CORE_PROBE_PASS "$out/probe.txt" || finish "MOBILE_CORE_FAIL probe"
inputs=$(grep -o 'input: [0-9]* events' "$out/agent_report.txt" | head -1 | cut -d' ' -f2)
[ "${inputs:-0}" -ge 6 ] || finish "MOBILE_CORE_FAIL the agent received ${inputs:-no} input events (6 sent)"
finish MOBILE_CORE_PASS
