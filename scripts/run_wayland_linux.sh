#!/usr/bin/env bash
# The Linux agent in a Wayland session (docs/LINUX.md), on the virtual Wayland display of
# scripts/wayland_display_linux.sh (headless sway as the user qqtest): the screen through the ScreenCast portal and
# PipeWire, input through wlroots' virtual pointer and the virtual keyboard. The viewer runs on Xvfb :92 and connects
# by IP (M2, a test build). Checks:
# - the picture: what the viewer shows (viewer_session.bmp) against the Wayland screen itself (grim), mean difference;
# - input: wev, a native Wayland program full screen, must see the viewer's input test (five pointer positions, F24
#   down and up), where XWayland's windows would not tell (the agent's own --verify-input asks X11).
# Prints the reports and WAYLAND_LINUX_PASS or WAYLAND_LINUX_FAIL <why>. Needs what wayland_display_linux.sh needs,
# wev, grim, Xvfb.
# Usage: scripts/run_wayland_linux.sh (CARGO_TARGET_DIR and SECONDS_RUN from the environment).
set -u
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
seconds=${SECONDS_RUN:-15}
out=/tmp/queqiao_wayland_test
rm -rf "$out"; mkdir -p "$out"; chmod 777 "$out"
started=$(date +%s)
nice -n 10 cargo build -q --release --features queqiao-agent/test-switches,queqiao-viewer/test-switches \
    -p queqiao-agent -p queqiao-viewer > "$out/build.log" 2>&1
code=$?
echo "build exit $code in $(( $(date +%s) - started )) s"
if [ "$code" -ne 0 ]; then grep -E "^error" -A6 "$out/build.log" | head -30; echo WAYLAND_LINUX_FAIL build; exit 0; fi
bin=$CARGO_TARGET_DIR/release
chmod 755 "$CARGO_TARGET_DIR" "$bin" 2> /dev/null

bash scripts/wayland_display_linux.sh status | grep -q WAYLAND_RUNNING || bash scripts/wayland_display_linux.sh start | tail -1
as_user() { runuser -u qqtest -- bash -c ". /tmp/queqiao_wayland/env; export HOME=/home/qqtest; $1"; }
# The Wayland screen: only wev, full screen, logging what it gets line by line.
as_user "pkill -u qqtest -x wev; pkill -u qqtest -f 'xclock|xev|xterm|foot'; true"
sleep 0.5
as_user "swaymsg exec 'stdbuf -oL wev > $out/wev.log 2>&1' > /dev/null"
sleep 1.5
as_user "swaymsg '[app_id=wev] fullscreen enable' > /dev/null"
sleep 0.5

pgrep -f "Xvfb :92 " > /dev/null || { Xvfb :92 -screen 0 1920x1080x24 -noreset > "$out/xvfb92.log" 2>&1 & }
sleep 1
port=$(( 47200 + RANDOM % 500 ))
DISPLAY=:92 "$bin/queqiao-viewer" --listen "127.0.0.1:$port" --seconds "$seconds" --report "$out/viewer.txt" \
    --input-test --width 960 --codec h264 --ui-pictures "$out" > "$out/viewer.out" 2>&1 &
viewer=$!
sleep 1
as_user "$bin/queqiao-agent --connect 127.0.0.1:$port --seconds $seconds --report $out/agent.txt > $out/agent.out 2>&1"
agent_code=$?
wait "$viewer"
viewer_code=$?
as_user "grim $out/wayland_screen.png"
as_user "pkill -u qqtest -x wev; true"
sleep 0.3
echo "=== agent (exit $agent_code)"; cat "$out/agent.txt" 2> /dev/null || tail -20 "$out/agent.out"
echo "=== viewer (exit $viewer_code)"; grep -vE "^[0-9]+ ms: (path|video in)" "$out/viewer.txt" 2> /dev/null | head -60 || tail -20 "$out/viewer.out"

why=''
grep -q "RESULT PASS" "$out/agent.txt" 2> /dev/null || why="$why agent"
grep -q "RESULT PASS" "$out/viewer.txt" 2> /dev/null || why="$why viewer"
grep -q "on Wayland" "$out/agent.txt" 2> /dev/null || why="$why not-the-portal"
# The picture: the viewer's against the screen's, both at 480x270, mean difference per channel (0-255).
picture=$(python3 - "$out" << 'EOF'
import sys
from PIL import Image
out = sys.argv[1]
try:
    a = Image.open(out + "/viewer_session.bmp").convert("RGB")
    b = Image.open(out + "/wayland_screen.png").convert("RGB")
except Exception as error:
    print("none", error)
    sys.exit()
# The viewer's window shows the picture scaled to its width with the aspect kept: compare the same 16:9 area.
a = a.resize((480, 270))
b = b.resize((480, 270))
pa, pb = a.load(), b.load()
total = sum(abs(pa[x, y][c] - pb[x, y][c]) for x in range(480) for y in range(270) for c in range(3))
print("%.1f" % (total / (480 * 270 * 3)))
EOF
)
echo "picture: mean difference $picture (viewer against the Wayland screen, 0-255)"
case "$picture" in none*) why="$why picture" ;; *) awk "BEGIN { exit !($picture < 12) }" || why="$why picture-differs" ;; esac
# Input: wev's pointer positions and the F24 key.
python3 - "$out/wev.log" << 'EOF' > "$out/input_check.txt"
import re, sys
text = open(sys.argv[1], errors="replace").read()
points = [(float(x), float(y)) for x, y in re.findall(r"motion: time: \d+; x, y: ([\d.]+), ([\d.]+)", text)]
expected = [(479, 269), (1439, 269), (1439, 809), (479, 809), (959, 539)]
found = sum(1 for ex, ey in expected if any(abs(x - ex) <= 2 and abs(y - ey) <= 2 for x, y in points))
keys = re.findall(r"key: (\d+); state: (\d)", text)
f24 = [state for key, state in keys if key == "202"]
print(f"input check (wev): pointer at the expected place {found}/5, F24 down {'1' in f24}, up {'0' in f24}")
print("INPUT_OK" if found == 5 and "1" in f24 and "0" in f24 else "INPUT_BAD")
EOF
cat "$out/input_check.txt"
grep -q INPUT_OK "$out/input_check.txt" || why="$why input"
if [ -z "$why" ]; then echo WAYLAND_LINUX_PASS; else echo "WAYLAND_LINUX_FAIL$why"; fi
