#!/usr/bin/env bash
# The Linux agent in a Wayland session (docs/LINUX.md), on the virtual Wayland display of
# scripts/wayland_display_linux.sh as the user qqtest. DESKTOP picks the desktop:
# - sway (the default): the screen through the ScreenCast portal and PipeWire, input through wlroots' virtual pointer
#   and keyboard. The test window is wev (a native Wayland program) full screen; the picture is checked against the
#   screen itself (grim).
# - gnome (mutter): the screen and the input through the RemoteDesktop portal, whose dialog scripts/portal_approve.py
#   answers as a person would. The test window is scripts/wayland_test_window.py, a native GTK window full screen and
#   white, started once the dialog is answered (so it is the newest window: in front, with the keyboard); the picture
#   must be that white. (xev, an X11 window through XWayland, saw only some of the moves there.)
# - kde (KWin, in the Ubuntu 24.04 root: NOBLE=1): KDE's portal. Plasma 5.27 asks no question of a program that is
#   not sandboxed (KDE bug 479013; Plasma 6 does ask): a dialog, if one comes, must be answered, none is fine. KWin's
#   virtual backend there records no frames for screen casts (its buffers are made, the stream runs, none comes,
#   whatever changes on screen), so the session's picture cannot be checked: the probe
#   (crates/queqiao-capture/examples/wayland_input_probe.rs) opens the same portal session as the agent and sends the
#   input test, which must reach the test window.
# The viewer runs on Xvfb :92 and connects by IP (M2, a test build); its input test (five pointer positions, a left
# click, F24 down and up) must reach the test window, which logs what it gets.
# Prints the reports and WAYLAND_LINUX_PASS or WAYLAND_LINUX_FAIL <why>.
# NOBLE=1 (GNOME, KDE): the desktop in the Ubuntu 24.04 root of scripts/noble_root_linux.sh (GNOME 46, whose portal keeps
# the person's answer); the first session starts without a kept answer and the dialog's "remember" is ticked where
# it has one. KEPT=1: a later session, which must start without a dialog, the agent offering the kept answer.
# Usage: [DESKTOP=gnome [NOBLE=1 [KEPT=1]]] scripts/run_wayland_linux.sh (CARGO_TARGET_DIR and SECONDS_RUN from the
# environment).
set -u
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
seconds=${SECONDS_RUN:-15}
desktop=${DESKTOP:-sway}
noble=${NOBLE:-}
kept=${KEPT:-}
py=/usr/bin/python3.10
[ -n "$noble" ] && py=/usr/bin/python3
if [ -n "$noble" ] && [ "$desktop" = sway ]; then echo "WAYLAND_LINUX_FAIL the 24.04 root has GNOME and KDE"; exit 0; fi
if [ "$desktop" = kde ] && [ -z "$noble" ]; then echo "WAYLAND_LINUX_FAIL KDE runs in the 24.04 root (NOBLE=1)"; exit 0; fi
out=/tmp/queqiao_wayland_test
rm -rf "$out"; mkdir -p "$out"; chmod 777 "$out"
started=$(date +%s)
nice -n 10 cargo build -q --release --features queqiao-agent/test-switches,queqiao-viewer/test-switches \
    -p queqiao-agent -p queqiao-viewer > "$out/build.log" 2>&1 \
    && nice -n 10 cargo build -q --release -p queqiao-capture --example wayland_input_probe >> "$out/build.log" 2>&1
code=$?
echo "build exit $code in $(( $(date +%s) - started )) s"
if [ "$code" -ne 0 ]; then grep -E "^error" -A6 "$out/build.log" | head -30; echo WAYLAND_LINUX_FAIL build; exit 0; fi
bin=$CARGO_TARGET_DIR/release
chmod 755 "$CARGO_TARGET_DIR" "$bin" 2> /dev/null
cp scripts/portal_approve.py scripts/wayland_test_window.py scripts/wayland_display_linux.sh "$out/" \
    && chmod 755 "$out/portal_approve.py" "$out/wayland_test_window.py" "$out/wayland_display_linux.sh"

# The display (in the 24.04 root: its own /tmp, so its own /tmp/queqiao_wayland), and commands on it as the test user.
display() {
    if [ -z "$noble" ]; then
        DESKTOP=$desktop bash scripts/wayland_display_linux.sh "$1"
        return
    fi
    # In the 24.04 root as the test user (the script's part for root would change user inside proot, which fails
    # there); GNOME's system bus first, as root. proot lasts while what it started runs: those start apart.
    if [ "$1" != start ]; then
        bash scripts/noble_root_linux.sh run "DESKTOP=$desktop bash $out/wayland_display_linux.sh $1"
        return
    fi
    if ! [ -S "${NOBLE_ROOT:-/tmp/noble}/run/dbus/system_bus_socket" ]; then
        (setsid bash scripts/noble_root_linux.sh root "mkdir -p /run/dbus && exec dbus-daemon --config-file=/etc/dbus-1/queqiao-system.conf --nofork" \
            > "$out/noble_bus.log" 2>&1 < /dev/null &)
        sleep 2
    fi
    # The other desktop stopped first (else it would answer the wait below), then this one, waited for by name.
    display stop > /dev/null
    (setsid bash scripts/noble_root_linux.sh run "QUEQIAO_PIPEWIRE_PRELOAD=/usr/local/lib/queqiao-proot-shim.so \
        DESKTOP=$desktop bash $out/wayland_display_linux.sh start" \
        > "$out/display.log" 2>&1 < /dev/null &)
    for _ in $(seq 60); do
        display status | grep -q "WAYLAND_RUNNING .* $desktop\$" && break
        sleep 1
    done
    display status
}
display status | grep -q "WAYLAND_RUNNING .* $desktop\$" || display start | tail -1
if [ -n "$noble" ]; then
    as_user() { bash scripts/noble_root_linux.sh run ". /tmp/queqiao_wayland/env; $1"; }
else
    as_user() { runuser -u qqtest -- bash -c ". /tmp/queqiao_wayland/env; export HOME=/home/qqtest; $1"; }
fi
# A program left running (with its redirections in $1). Started apart here, not inside the command: proot (the 24.04
# root) lasts while what it runs does, so a program put in the background inside it held the test up until it ended.
as_user_bg() {
    if [ -n "$noble" ]; then
        (setsid bash scripts/noble_root_linux.sh run ". /tmp/queqiao_wayland/env; exec $1" < /dev/null > /dev/null 2>&1 &)
    else
        as_user "(setsid $1 < /dev/null &)"
    fi
}
as_user "pkill -u qqtest -x wev; pkill -u qqtest -x xev; pkill -u qqtest -f 'xclock|xterm|wayland_test_window'; true"
sleep 0.5
still=''
if [ "$desktop" = sway ]; then
    # A still screen with no window: a session's first picture must come all the same. wlroots' portal (0.5) asks
    # for each frame "with damage" since its last copy, the first too: a second capture of a screen unchanged since
    # the first got none, until the capture damaged the screen without a visible change (wayland_nudge.rs). Two
    # captures without input; the second must have pictures (before that: "no picture within 5 s").
    sleep 1
    as_user "$bin/examples/wayland_input_probe 1 50 still" > "$out/still_first.txt" 2>&1
    as_user "$bin/examples/wayland_input_probe 1 50 still" > "$out/still.txt" 2>&1
    grep -qE "^frames before input: [1-9]" "$out/still.txt" || still=bad
    echo "still screen: $(grep -E "^frames before input|PROBE_FAIL" "$out/still.txt" | head -1)"
    as_user "swaymsg exec 'stdbuf -oL wev > $out/events.log 2>&1' > /dev/null"
    sleep 1.5
    as_user "swaymsg '[app_id=wev] fullscreen enable' > /dev/null"
else
    # GNOME's dialog, answered as the person at the PC would (it must be in sight: a covered window gets no frames, and
    # its switch never turns), "remember" ticked where it has one. Then the test window, the newest: in front and with
    # the keyboard. A first session starts without a kept answer; with KEPT no dialog may come (the helper still
    # answers one that does, so the rest of the session runs), and the test window starts after the agent's main window
    # (no tray there) opened, so that it is the newest again.
    [ -z "$kept" ] && as_user "rm -f \$XDG_CONFIG_HOME/queqiao/portal-*.token"
    as_user_bg "$py $out/portal_approve.py $(if [ -n "$kept" ]; then echo 15; elif [ "$desktop" = kde ]; then echo 12; else echo 40; fi) --remember > $out/approve.txt 2>&1"
    cat > "$out/window.sh" << 'EOF'
# $1 the output folder, $2 the Python, $3 "wait" for the dialog's answer first, or the seconds to wait.
if [ "$3" = wait ]; then
    for _ in $(seq 400); do
        grep -q APPROVED "$1/approve.txt" 2> /dev/null && break
        sleep 0.1
    done
    sleep 1
else
    sleep "$3"
fi
# Its Wayland protocol log too: GTK drops keys without a symbol in the keymap (F24, the test key, is one).
WAYLAND_DEBUG=client GDK_BACKEND=wayland exec "$2" "$1/wayland_test_window.py" "$1/events.log" 60 2> "$1/protocol.log"
EOF
    as_user_bg "bash $out/window.sh $out $py $(if [ "$desktop" = kde ]; then echo 0; elif [ -n "$kept" ]; then echo 5; else echo wait; fi) \
        > $out/window.err 2>&1"
fi
sleep 0.5

pgrep -f "Xvfb :92 " > /dev/null || { Xvfb :92 -screen 0 1920x1080x24 -noreset > "$out/xvfb92.log" 2>&1 & }
sleep 1
# The test clicks at its last point before its key (the buttons' route too). On GNOME the test window starts once the
# dialog is answered: the test starts later, after the picture (5 s): the picture then shows a still screen after the
# session bar tucked away, which the viewer must paint again where the bar was (nothing new comes from the remote).
input_at=${INPUT_AT:-3}
[ "$desktop" != sway ] && input_at=${INPUT_AT:-6}
if [ "$desktop" = kde ]; then
    # The probe: the portal session, then the input test (five places, a click, F24) after 3 s.
    sleep 3
    as_user "timeout 60 $bin/examples/wayland_input_probe 3 400 > $out/probe.out 2>&1"
    sleep 1
    as_user "pkill -u qqtest -f wayland_test_window; true"
    # The dialog watcher's verdict (it watches 12 s).
    for _ in $(seq 30); do
        [ -s "$out/approve.txt" ] && break
        sleep 0.5
    done
    echo "=== probe"; grep -vE "libEGL|pci id|^MouseMove|^MouseButton|^Key" "$out/probe.out"
    [ -f "$out/approve.txt" ] && echo "dialog: $(cat "$out/approve.txt")"
else
port=$(( 47200 + RANDOM % 500 ))
DISPLAY=:92 "$bin/queqiao-viewer" --listen "127.0.0.1:$port" --seconds "$(( seconds + 10 ))" --report "$out/viewer.txt" \
    --input-test --input-test-click --input-test-at $input_at --picture-at ${PICTURE_AT:-5} --width 960 --codec h264 --ui-pictures "$out" > "$out/viewer.out" 2>&1 &
viewer=$!
sleep 1
# A stalled agent ends the test, not hangs it.
as_user "timeout $(( seconds + 40 )) $bin/queqiao-agent --connect 127.0.0.1:$port --seconds $(( seconds + 10 )) --report $out/agent.txt > $out/agent.out 2>&1"
agent_code=$?
wait "$viewer"
viewer_code=$?
[ "$desktop" = sway ] && as_user "grim $out/wayland_screen.png"
as_user "pkill -u qqtest -x wev; pkill -u qqtest -f wayland_test_window; true"
sleep 0.3
echo "=== agent (exit $agent_code)"; cat "$out/agent.txt" 2> /dev/null || tail -20 "$out/agent.out"
echo "=== viewer (exit $viewer_code)"; grep -vE "^[0-9]+ ms: (path|video in)" "$out/viewer.txt" 2> /dev/null | head -60 || tail -20 "$out/viewer.out"
[ -f "$out/approve.txt" ] && echo "dialog: $(cat "$out/approve.txt")"
fi

why=''
[ -n "$still" ] && why="$why still-screen"
if [ "$desktop" = kde ]; then
    grep -q "^PROBE_DONE" "$out/probe.out" 2> /dev/null || why="$why probe"
    grep -q "^capture: RemoteDesktop portal" "$out/probe.out" 2> /dev/null || why="$why not-remote-desktop"
    grep -q "^input through the RemoteDesktop portal" "$out/probe.out" 2> /dev/null || why="$why input-not-portal"
    # A dialog that came must have been answered; none is Plasma 5.27's way for programs not sandboxed.
    grep -qE "APPROVED|no portal dialog" "$out/approve.txt" 2> /dev/null || why="$why dialog-unanswered"
    echo "picture: not checked (KWin's virtual backend records no frames for screen casts)"
else
grep -q "RESULT PASS" "$out/agent.txt" 2> /dev/null || why="$why agent"
grep -q "RESULT PASS" "$out/viewer.txt" 2> /dev/null || why="$why viewer"
grep -q "on Wayland" "$out/agent.txt" 2> /dev/null || why="$why not-the-portal"
if [ "$desktop" != sway ]; then
    grep -q "captured through the RemoteDesktop portal" "$out/agent.txt" 2> /dev/null || why="$why not-remote-desktop"
    if [ -n "$kept" ]; then
        grep -q APPROVED "$out/approve.txt" 2> /dev/null && why="$why asked-again"
        grep -q "earlier answer offered" "$out/agent.txt" 2> /dev/null || why="$why no-kept-answer"
    else
        grep -q APPROVED "$out/approve.txt" 2> /dev/null || why="$why not-approved"
    fi
fi
# The picture: sway's against the screen (grim); GNOME's and KDE's must be the test window's white.
picture=$(python3 - "$out" "$desktop" << 'EOF'
import sys
from PIL import Image
out, desktop = sys.argv[1], sys.argv[2]
try:
    a = Image.open(out + "/viewer_session.bmp").convert("RGB").resize((480, 270))
    b = Image.open(out + "/wayland_screen.png").convert("RGB").resize((480, 270)) if desktop == "sway" else Image.new("RGB", (480, 270), (255, 255, 255))
except Exception as error:
    print("none", error)
    sys.exit()
pa, pb = a.load(), b.load()
# The bar's tab at the top centre is Queqiao's own: left out.
cells = [(x, y) for x in range(480) for y in range(270) if not (200 <= x < 280 and y < 12)]
total = sum(abs(pa[x, y][c] - pb[x, y][c]) for x, y in cells for c in range(3))
print("%.1f" % (total / (len(cells) * 3)))
EOF
)
echo "picture: mean difference $picture (viewer against the $desktop screen, 0-255)"
case "$picture" in none*) why="$why picture" ;; *) awk "BEGIN { exit !($picture < 12) }" || why="$why picture-differs" ;; esac
fi
# Input: the test window's pointer positions (full screen: screen positions), the click, and F24: wev logs all three
# (keys as X key codes, 202); on GNOME the GTK window logs the pointer and the click, its protocol log the keys (Linux
# codes, 194).
/usr/bin/python3.10 - "$out/events.log" "$desktop" "$out/protocol.log" << 'EOF' > "$out/input_check.txt"
import re, sys
text = open(sys.argv[1], errors="replace").read()
if sys.argv[2] == "sway":
    points = [(float(x), float(y)) for x, y in re.findall(r"motion: time: \d+; x, y: ([\d.]+), ([\d.]+)", text)]
    keys = [(key, state) for key, state in re.findall(r"key: (\d+); state: (\d)", text)]
    down = any(key == "202" and state == "1" for key, state in keys)
    up = any(key == "202" and state == "0" for key, state in keys)
    buttons = re.findall(r"button: (\d+) \(\w+\), state: (\d)", text)
    clicked = ("272", "1") in buttons and ("272", "0") in buttons
else:
    points = [(float(x), float(y)) for x, y in re.findall(r"^motion (\d+) (\d+)$", text, re.M)]
    clicked = re.search(r"^button 1 pressed$", text, re.M) and re.search(r"^button 1 released$", text, re.M)
    try:
        protocol = open(sys.argv[3], errors="replace").read()
    except OSError:
        protocol = ""
    keys = re.findall(r"wl_keyboard@\d+\.key\(\d+, \d+, (\d+), (\d)\)", protocol)
    down = ("194", "1") in keys
    up = ("194", "0") in keys
expected = [(479, 269), (1439, 269), (1439, 809), (479, 809), (959, 539)]
found = sum(1 for ex, ey in expected if any(abs(x - ex) <= 2 and abs(y - ey) <= 2 for x, y in points))
print(f"input check ({'wev' if sys.argv[2] == 'sway' else 'the GTK test window'}): pointer at the expected place {found}/5, click {bool(clicked)}, F24 down {down}, up {up}")
print("INPUT_OK" if found == 5 and down and up and clicked else "INPUT_BAD")
EOF
cat "$out/input_check.txt"
grep -q INPUT_OK "$out/input_check.txt" || why="$why input"
if [ -z "$why" ]; then echo WAYLAND_LINUX_PASS; else echo "WAYLAND_LINUX_FAIL$why"; fi
