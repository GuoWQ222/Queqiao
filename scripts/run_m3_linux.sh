#!/usr/bin/env bash
# M3 on Linux (docs/LINUX.md step 5): the Linux agent and viewer with an account, through a queqiao-server of their
# own on 127.0.0.1, as scripts\run_m4_local.ps1 does on Windows: the server made (init, a user), the agent logged in and
# online, the viewer logged in and connected to it by name (--device), with the input test. Sessions (SESSIONS, default
# "direct allowed declined ui"): direct (the clipboard both ways too; the session bar and card of figures, pictured with
# the bar out and tucked away, its Figures button clicked twice; a command line on the agent in a terminal window
# here, detached and shown again: --terminal-test), allowed (the agent asks the person, who allows:
# the consent prompt answered by the test; pictures of the banner and the prompt), declined (who declines: neither side passes, both must say so), ui (the
# viewer through its login form and device list as a person would, filled and pressed by the program, with pictures of
# both: login.bmp and devices.bmp). Then the session's forms no session opens by itself (the transfer list, the
# password form), by examples/linux_forms_probe, with pictures (transfers.bmp, password.bmp). main: the agent alone,
# its main window's pages pictured (main_devices.bmp ...). h265: the viewer asks for H.265 (NVENC on the agent, NVDEC
# on the viewer). h265_444: 4:4:4 10-bit asked for (the agent makes 8-bit 4:4:4).
# The agent's screen is Xvfb :91 with a clock and `top` (so the picture changes); the viewer's window is on Xvfb :92.
# Everything the programs keep (the device's key, the saved logins, the single-instance lock) goes into the test's own
# folder (XDG_CONFIG_HOME, XDG_STATE_HOME, XDG_RUNTIME_DIR), never the person's ~/.config/queqiao.
# Prints each session's reports and M3_LINUX_PASS or M3_LINUX_FAIL <why>. Needs Xvfb, xclock, xterm.
# Usage: scripts/run_m3_linux.sh (from anywhere; CARGO_TARGET_DIR, SECONDS_RUN and SESSIONS from the environment).
set -u
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
seconds=${SECONDS_RUN:-20}
sessions=${SESSIONS:-direct allowed declined ui main h265 h265_444}
out=/tmp/queqiao_m3
rm -rf "$out"; mkdir -p "$out"
export XDG_CONFIG_HOME=$out/config XDG_STATE_HOME=$out/state XDG_RUNTIME_DIR=$out/run
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
mkdir -m 700 "$XDG_RUNTIME_DIR"
started=$(date +%s)
nice -n 10 cargo build -q --release --features queqiao-agent/test-switches,queqiao-viewer/test-switches \
    -p queqiao-agent -p queqiao-viewer -p queqiao-server > "$out/build.log" 2>&1 \
    && nice -n 10 cargo build -q --release -p queqiao-display --example linux_forms_probe >> "$out/build.log" 2>&1
code=$?
echo "build exit $code in $(( $(date +%s) - started )) s"
if [ "$code" -ne 0 ]; then grep -E "^error" -A6 "$out/build.log" | head -30; echo M3_LINUX_FAIL build; exit 0; fi
bin=$CARGO_TARGET_DIR/release
for display in 91 92; do
    pgrep -f "Xvfb :$display " > /dev/null || { Xvfb ":$display" -screen 0 1920x1080x24 -noreset > "$out/xvfb$display.log" 2>&1 & }
done
sleep 1
DISPLAY=:91 xclock -update 1 -geometry 300x300+100+100 > /dev/null 2>&1 &
clock=$!
DISPLAY=:91 xterm -geometry 100x30+500+100 -e "top -d 0.5" > /dev/null 2>&1 &
term=$!

# The server: a new data folder, its certificate's fingerprint (pinned by the clients) and one user.
data=$out/server
init=$("$bin/queqiao-server" init --data "$data" 2>&1)
pin=$(printf '%s\n' "$init" | grep -o 'fingerprint: [0-9a-f]\{64\}' | head -1 | cut -d' ' -f2)
if [ -z "$pin" ]; then echo "$init"; echo M3_LINUX_FAIL server-init; kill "$clock" "$term" 2> /dev/null; exit 0; fi
password=$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')
printf '%s\n' "$password" | "$bin/queqiao-server" add-user alice --data "$data" > "$out/add_user.txt" 2>&1
port=$(( 47600 + RANDOM % 300 ))
listen=127.0.0.1:$port
"$bin/queqiao-server" serve --listen "$listen" --data "$data" --public-ip 127.0.0.1 --turn-port 0 \
    > "$out/server.txt" 2>&1 &
server=$!
up=''
for _ in $(seq 50); do
    if (exec 3<> "/dev/tcp/127.0.0.1/$port") 2> /dev/null; then up=yes; break; fi
    sleep 0.2
done
if [ -z "$up" ]; then cat "$out/server.txt"; echo M3_LINUX_FAIL server; kill "$server" "$clock" "$term" 2> /dev/null; exit 0; fi
device=$(cat /proc/sys/kernel/hostname)
account="--server $listen --pin $pin --user alice --password $password"
why=''
for label in $sessions; do
    case $label in
        direct) agent_extra='--clipboard-test --dry-run-commands'; viewer_extra="--clipboard-test --command lock --figures --ui-pictures $out --terminal-test";
                agent_expects='test text from the other side received: yes'; viewer_expects='test text from the other side received: yes'; both_pass=yes ;;
        # The agent's banner and consent prompt pictured.
        allowed) agent_extra="--ask --answer allow --ui-pictures $out"; viewer_extra='';
                 agent_expects='the person at this PC allowed'; viewer_expects='RESULT PASS'; both_pass=yes ;;
        # Declined, the agent stays online until --seconds (a later --seconds wins): 30 s, then it writes its report.
        declined) agent_extra='--ask --answer deny --seconds 30'; viewer_extra='';
                  agent_expects='the person at this PC declined'; viewer_expects='the person at that PC declined'; both_pass='' ;;
        # The agent alone (no viewer) for 10 s: its main window, each page pictured 3 s on.
        main) agent_extra="--test-main-window --ui-pictures $out --seconds 10"; viewer_extra='';
              agent_expects='main window: pictures of its pages saved'; viewer_expects=''; both_pass=yes ;;
        # H.265 asked for: NVENC makes it, NVDEC decodes it (where the GPU has them; the H.264 sessions use NVDEC too).
        h265) agent_extra=''; viewer_extra='--codec h265';
              agent_expects='command from the viewer: picture: H.265'; viewer_expects='the picture is H.265 now'; both_pass=yes ;;
        # H.265 4:4:4 10-bit asked for, as Windows' h265 session: the Linux agent makes 4:4:4 at 8 bits (its NVENC path
        # takes no 10-bit yet), NVDEC decodes it into three planes.
        h265_444) agent_extra=''; viewer_extra='--codec h265 --chroma444 --ten-bit';
              agent_expects='command from the viewer: picture: H.265 4:4:4'; viewer_expects='the picture is H.265 now'; both_pass=yes ;;
        ui) agent_extra=''; viewer_extra='';
            agent_expects='RESULT PASS'; viewer_expects='sessions from the device list'; both_pass=yes ;;
        *) echo "unknown session $label"; why="$why unknown-$label"; continue ;;
    esac
    # One session, where a viewer comes (main has none).
    agent_sessions=(--sessions 1)
    [ "$label" = main ] && agent_sessions=()
    # shellcheck disable=SC2086
    DISPLAY=:91 "$bin/queqiao-agent" $account "${agent_sessions[@]}" --seconds $(( seconds + 60 )) --verify-input \
        --report "$out/agent_$label.txt" $agent_extra > "$out/agent_$label.out" 2>&1 &
    agent=$!
    sleep 2
    # shellcheck disable=SC2086
    target=(--device "$device")
    [ "$label" = ui ] && target=(--ui-test "$device")
    viewer_code=none
    if [ "$label" != main ]; then
    DISPLAY=:92 "$bin/queqiao-viewer" $account "${target[@]}" --seconds "$seconds" --input-test --width 960 \
        --codec h264 --report "$out/viewer_$label.txt" $viewer_extra > "$out/viewer_$label.out" 2>&1
    viewer_code=$?
    fi
    # The agent leaves after its one session, or at its --seconds.
    for _ in $(seq $(( (seconds + 70) * 5 ))); do kill -0 "$agent" 2> /dev/null || break; sleep 0.2; done
    kill "$agent" 2> /dev/null
    wait "$agent"
    agent_code=$?
    echo "=== session '$label': agent exit $agent_code, viewer exit $viewer_code"
    echo "--- agent"; cat "$out/agent_$label.txt" 2> /dev/null || tail -20 "$out/agent_$label.out"
    echo "--- viewer"; cat "$out/viewer_$label.txt" 2> /dev/null || tail -20 "$out/viewer_$label.out"
    grep -q "$agent_expects" "$out/agent_$label.txt" 2> /dev/null || why="$why $label-agent"
    if [ -n "$viewer_expects" ]; then
        grep -q "$viewer_expects" "$out/viewer_$label.txt" 2> /dev/null || why="$why $label-viewer"
    fi
    if [ "$label" = direct ]; then
        for picture in viewer_session viewer_session_tucked; do
            [ -s "$out/$picture.bmp" ] && echo "picture: $picture.bmp" || why="$why direct-$picture-picture"
        done
        grep -qE "^bar: no Figures button|^overlay: " "$out/viewer_direct.txt" && why="$why direct-bar"
        grep -q "^terminal test: PASS" "$out/viewer_direct.txt" || why="$why direct-terminal"
    fi
    pictures=''
    [ "$label" = allowed ] && pictures='banner prompt'
    [ "$label" = main ] && pictures='main_devices main_help main_transfers main_settings'
    for picture in $pictures; do
        [ -s "$out/$picture.bmp" ] && echo "picture: $picture.bmp" || why="$why $label-$picture-picture"
    done
    if [ "$label" = ui ]; then
        for picture in login devices; do
            [ -s "$out/$picture.bmp" ] && echo "picture: $picture.bmp $(stat -c %s "$out/$picture.bmp") bytes" || why="$why ui-$picture-picture"
        done
    fi
    if [ -n "$both_pass" ]; then
        grep -q "RESULT PASS" "$out/agent_$label.txt" 2> /dev/null || why="$why $label-agent-result"
    fi
done
kill "$server" 2> /dev/null
echo "=== forms"
DISPLAY=:92 timeout 60 "$bin/examples/linux_forms_probe" "$out" | tee "$out/forms.txt"
grep -q FORMS_PASS "$out/forms.txt" || why="$why forms"
kill "$clock" "$term" 2> /dev/null
if [ -z "$why" ]; then echo M3_LINUX_PASS; else echo "M3_LINUX_FAIL$why"; fi
