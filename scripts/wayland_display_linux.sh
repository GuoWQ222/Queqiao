#!/usr/bin/env bash
# A virtual Wayland display for tests (docs/LINUX.md): sway with its headless backend (software rendering, no GPU or
# screen needed), XWayland, and around it what a Wayland desktop session has for remote control: a session D-Bus,
# PipeWire and WirePlumber, xdg-desktop-portal with the wlroots backend (xdg-desktop-portal-wlr, set to share the one
# output without a dialog, so tests run unattended). Everything lives in its own runtime folder.
# Usage: scripts/wayland_display_linux.sh start | stop | status. `start` prints the environment to use (also kept in
#   $folder/env: `. /tmp/queqiao_wayland/env` before running a program on the display).
# Needs: sway, xwayland, pipewire, wireplumber, xdg-desktop-portal, xdg-desktop-portal-wlr, dbus.
set -u
folder=${QUEQIAO_WAYLAND_FOLDER:-/tmp/queqiao_wayland}
size=${WAYLAND_SIZE:-1920x1080}

# sway runs as an ordinary user only (it refuses root), as a desktop session does: started as root, this runs again as
# the test user $WAYLAND_USER (qqtest, made if missing), from a copy the user can read. Programs tested on the display
# run as that user too (`runuser -u qqtest -- ...`).
if [ "$(id -u)" = 0 ]; then
    user=${WAYLAND_USER:-qqtest}
    id "$user" > /dev/null 2>&1 || useradd -m -s /bin/bash "$user"
    # A folder left by someone else (an earlier run as root) would keep the user out.
    if [ -e "$folder" ] && [ "$(stat -c %U "$folder")" != "$user" ]; then
        rm -rf "$folder"
    fi
    copy=/tmp/queqiao_wayland_display.sh
    cp "$0" "$copy" && chmod 755 "$copy"
    exec runuser -u "$user" -- env QUEQIAO_WAYLAND_FOLDER="$folder" WAYLAND_SIZE="$size" bash "$copy" "$@"
fi

stop() {
    if [ -f "$folder/pids" ]; then
        # The newest first: the portals, PipeWire, sway, then the bus.
        tac "$folder/pids" | while read -r pid; do kill "$pid" 2> /dev/null; done
        sleep 1
        tac "$folder/pids" | while read -r pid; do kill -9 "$pid" 2> /dev/null; done
        rm -f "$folder/pids"
    fi
}

case "${1:-}" in
    stop) stop; echo WAYLAND_STOPPED; exit 0 ;;
    status)
        if [ -f "$folder/env" ] && . "$folder/env" && [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]; then
            echo "WAYLAND_RUNNING $WAYLAND_DISPLAY"
        else
            echo WAYLAND_NOT_RUNNING
        fi
        exit 0 ;;
    start) ;;
    *) echo "usage: $0 start | stop | status"; exit 2 ;;
esac

stop
rm -rf "$folder"
mkdir -p -m 700 "$folder/run"
mkdir -p "$folder/config/sway" "$folder/config/xdg-desktop-portal-wlr" "$folder/log"
export XDG_RUNTIME_DIR=$folder/run XDG_CONFIG_HOME=$folder/config XDG_CURRENT_DESKTOP=sway XDG_SESSION_TYPE=wayland
cat > "$folder/config/sway/config" << EOF
output HEADLESS-1 resolution $size position 0 0
xwayland enable
default_border none
EOF
cat > "$folder/config/xdg-desktop-portal-wlr/config" << EOF
[screencast]
chooser_type=none
output_name=HEADLESS-1
max_fps=60
EOF

# The session bus.
eval "$(dbus-launch --sh-syntax)"
echo "$DBUS_SESSION_BUS_PID" >> "$folder/pids"

# sway, headless: one virtual output, software rendering (pixman), no input devices. --unsupported-gpu: sway refuses
# to start where NVIDIA's driver is loaded, though the headless backend and pixman never use the GPU.
WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
    setsid sway --unsupported-gpu --config "$folder/config/sway/config" > "$folder/log/sway.log" 2>&1 < /dev/null &
echo $! >> "$folder/pids"
for _ in $(seq 100); do
    display=$(cd "$XDG_RUNTIME_DIR" && ls wayland-* 2> /dev/null | grep -v '\.lock$' | head -1)
    [ -n "$display" ] && break
    sleep 0.1
done
if [ -z "${display:-}" ]; then
    tail -20 "$folder/log/sway.log"
    echo WAYLAND_FAIL sway
    stop
    exit 1
fi
export WAYLAND_DISPLAY=$display
export SWAYSOCK=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock 2> /dev/null | head -1)
dbus-update-activation-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR XDG_CONFIG_HOME XDG_SESSION_TYPE 2> /dev/null

# PipeWire and its session manager; then the portals (started here rather than by D-Bus, so they get this
# environment and their logs land in the folder).
for program in pipewire wireplumber; do
    setsid "$program" > "$folder/log/$program.log" 2>&1 < /dev/null &
    echo $! >> "$folder/pids"
    sleep 0.5
done
setsid /usr/libexec/xdg-desktop-portal-wlr -l DEBUG > "$folder/log/portal-wlr.log" 2>&1 < /dev/null &
echo $! >> "$folder/pids"
sleep 0.5
setsid /usr/libexec/xdg-desktop-portal -v > "$folder/log/portal.log" 2>&1 < /dev/null &
echo $! >> "$folder/pids"
sleep 1

cat > "$folder/env" << EOF
export XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR
export XDG_CONFIG_HOME=$XDG_CONFIG_HOME
export XDG_CURRENT_DESKTOP=sway
export XDG_SESSION_TYPE=wayland
export WAYLAND_DISPLAY=$WAYLAND_DISPLAY
export SWAYSOCK=$SWAYSOCK
export DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS
export DISPLAY=$(grep -o 'DISPLAY=:[0-9]*' "$folder/log/sway.log" | tail -1 | cut -d= -f2)
EOF
cat "$folder/env"
echo "WAYLAND_RUNNING $WAYLAND_DISPLAY"
