#!/usr/bin/env bash
# M2 on Linux (docs/LINUX.md step 3): the Linux agent and viewer by IP address over 127.0.0.1, with what
# scripts\run_m4_local.ps1's direct session checks on Windows: video, the input test (--input-test / --verify-input),
# the clipboard both ways (text and a test picture), files both ways, the viewer's commands (dry run), the speakers
# muted, and sound (a test tone played into the agent's output, heard at the viewer). The agent's screen is Xvfb :91
# with a clock and `top` in an xterm (so the picture changes); the viewer's window is on Xvfb :92 (not captured by
# the agent). Sound: PulseAudio with two null sinks, qq_agent_out and qq_viewer_out (nothing is heard). Prints both
# reports and M2_LINUX_PASS or M2_LINUX_FAIL <why>. Needs Xvfb, xclock, xterm, PulseAudio (pulseaudio, pactl).
# Usage: scripts/run_m2_linux.sh (from anywhere; CARGO_TARGET_DIR, SECONDS_RUN and MUTE_ARG= are taken from the
# environment; MUTE_ARG= leaves the speakers alone).
set -u
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
seconds=${SECONDS_RUN:-20}
out=/tmp/queqiao_m2
rm -rf "$out"; mkdir -p "$out"
started=$(date +%s)
nice -n 10 cargo build -q --release --features queqiao-agent/test-switches,queqiao-viewer/test-switches,queqiao-service/test-switches \
    -p queqiao-agent -p queqiao-viewer -p queqiao-service > "$out/build.log" 2>&1
code=$?
echo "build exit $code in $(( $(date +%s) - started )) s"
if [ "$code" -ne 0 ]; then grep -E "^error" -A6 "$out/build.log" | head -30; echo M2_LINUX_FAIL build; exit 0; fi
bin=$CARGO_TARGET_DIR/release
for display in 91 92; do
    pgrep -f "Xvfb :$display " > /dev/null || { Xvfb ":$display" -screen 0 1920x1080x24 -noreset > "$out/xvfb$display.log" 2>&1 & }
done
# PulseAudio for this user (root in the container: it warns and runs), with the two null sinks.
pulseaudio --check 2> /dev/null || pulseaudio --start --exit-idle-time=-1 --log-target=file:"$out/pulse.log" 2> /dev/null
for sink in qq_agent_out qq_viewer_out; do
    pactl list short sinks 2> /dev/null | grep -q "	$sink	" || pactl load-module module-null-sink "sink_name=$sink" > /dev/null
done
sleep 1
DISPLAY=:91 xclock -update 1 -geometry 300x300+100+100 > /dev/null 2>&1 &
clock=$!
DISPLAY=:91 xterm -geometry 100x30+500+100 -e "top -d 0.5" > /dev/null 2>&1 &
term=$!
sleep 1
head -c 300000 /dev/urandom > "$out/to-viewer.bin"
head -c 200000 /dev/urandom > "$out/to-agent.bin"
port=$(( 47200 + RANDOM % 500 ))
DISPLAY=:92 "$bin/queqiao-viewer" --listen "127.0.0.1:$port" --seconds "$seconds" --report "$out/viewer.txt" \
    --input-test --width 960 --clipboard-test ${MUTE_ARG---mute-speakers} --send-file "$out/to-agent.bin" \
    --command ctrl-alt-del --command lock --command restart --codec h264 \
    --sound-device qq_viewer_out --sound-test > "$out/viewer.out" 2>&1 &
viewer=$!
sleep 1
DISPLAY=:91 "$bin/queqiao-agent" --connect "127.0.0.1:$port" --seconds "$seconds" --report "$out/agent.txt" \
    --verify-input --clipboard-test --dry-run-commands --send-file "$out/to-viewer.bin" \
    --sound-device qq_agent_out --sound-test > "$out/agent.out" 2>&1
agent_code=$?
wait "$viewer"
viewer_code=$?
kill "$clock" "$term" 2> /dev/null
echo "=== agent (exit $agent_code)"; cat "$out/agent.txt" 2> /dev/null || cat "$out/agent.out"
echo "=== viewer (exit $viewer_code)"; cat "$out/viewer.txt" 2> /dev/null || cat "$out/viewer.out"
why=''
grep -q "RESULT PASS" "$out/agent.txt" 2> /dev/null || why="$why agent"
grep -q "RESULT PASS" "$out/viewer.txt" 2> /dev/null || why="$why viewer"
for side in agent viewer; do
    grep -q "test text from the other side received: yes" "$out/$side.txt" 2> /dev/null || why="$why $side-clipboard"
done
if [ -z "$why" ]; then echo M2_LINUX_PASS; else echo "M2_LINUX_FAIL$why"; fi
