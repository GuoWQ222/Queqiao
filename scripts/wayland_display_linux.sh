#!/usr/bin/env bash
# A virtual Wayland display for tests (docs/LINUX.md), with what a Wayland desktop session has for remote control: a
# session D-Bus, PipeWire and WirePlumber, xdg-desktop-portal and the desktop's own portal backend. DESKTOP picks it:
# - sway (the default): sway with its headless backend (software rendering), XWayland, xdg-desktop-portal-wlr set to
#   share the one output without a dialog;
# - gnome: mutter, GNOME's compositor, headless on a virtual monitor (software rendering), XWayland,
#   xdg-desktop-portal-gnome: ScreenCast and RemoteDesktop, whose dialog a person answers (the tests answer it with
#   scripts/portal_approve.py). GNOME Shell itself needs logind, which a container without systemd lacks; the portals
#   and the compositor's screen-cast and remote-desktop services are mutter's, as under GNOME Shell.
# - kde: KWin, KDE's compositor, on its virtual backend (software rendering; no XWayland), xdg-desktop-portal-kde:
#   ScreenCast and RemoteDesktop through KWin's own protocols, and KDE's dialog (answered the same way; Qt's
#   accessibility is turned on for it). Plasma's shell is not needed for those.
# Everything lives in its own runtime folder, as the test user (sway refuses root; the same user for the others).
# Usage: [DESKTOP=gnome|kde] scripts/wayland_display_linux.sh start | stop | status. `start` prints the environment
#   to use (also kept in $folder/env: `. /tmp/queqiao_wayland/env` before running a program on the display).
# Needs: sway, xdg-desktop-portal-wlr (sway); mutter, xdg-desktop-portal-gnome (gnome); kwin-wayland,
#   xdg-desktop-portal-kde (kde: scripts/noble_root_linux.sh has them); xwayland, pipewire, wireplumber,
#   xdg-desktop-portal, dbus.
set -u
folder=${QUEQIAO_WAYLAND_FOLDER:-/tmp/queqiao_wayland}
size=${WAYLAND_SIZE:-1920x1080}
desktop=${DESKTOP:-sway}

# Started as root, this runs again as the test user $WAYLAND_USER (qqtest, made if missing), from a copy the user can
# read. Programs tested on the display run as that user too (`runuser -u qqtest -- ...`). GNOME's services reach for
# the system bus: a container without one gets one here.
if [ "$(id -u)" = 0 ]; then
    user=${WAYLAND_USER:-qqtest}
    id "$user" > /dev/null 2>&1 || useradd -m -s /bin/bash "$user"
    # A folder left by someone else (an earlier run as root) would keep the user out.
    if [ -e "$folder" ] && [ "$(stat -c %U "$folder")" != "$user" ]; then
        rm -rf "$folder"
    fi
    if [ "$desktop" != sway ] && ! [ -S /run/dbus/system_bus_socket ]; then
        mkdir -p /run/dbus && dbus-daemon --system --fork
    fi
    copy=/tmp/queqiao_wayland_display.sh
    cp "$0" "$copy" && chmod 755 "$copy"
    exec runuser -u "$user" -- env QUEQIAO_WAYLAND_FOLDER="$folder" WAYLAND_SIZE="$size" DESKTOP="$desktop" \
        bash "$copy" "$@"
fi

stop() {
    if [ -f "$folder/pids" ]; then
        # The newest first: the portals, PipeWire, the compositor, then the bus.
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
            echo "WAYLAND_RUNNING $WAYLAND_DISPLAY $QUEQIAO_DESKTOP"
        else
            echo WAYLAND_NOT_RUNNING
        fi
        exit 0 ;;
    start) ;;
    *) echo "usage: [DESKTOP=sway|gnome|kde] $0 start | stop | status"; exit 2 ;;
esac
case "$desktop" in sway | gnome | kde) ;; *) echo "WAYLAND_FAIL no desktop $desktop (sway, gnome or kde)"; exit 2 ;; esac

stop
rm -rf "$folder"
mkdir -p -m 700 "$folder/run"
mkdir -p "$folder/config/sway" "$folder/config/xdg-desktop-portal-wlr" "$folder/log"
export XDG_RUNTIME_DIR=$folder/run XDG_CONFIG_HOME=$folder/config XDG_SESSION_TYPE=wayland
case "$desktop" in
    sway) export XDG_CURRENT_DESKTOP=sway ;;
    gnome) export XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_DESKTOP=gnome ;;
    # Qt's accessibility on for every Qt program here: the tests answer KDE's dialog through it.
    kde) export XDG_CURRENT_DESKTOP=KDE XDG_SESSION_DESKTOP=KDE KDE_FULL_SESSION=true QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1 ;;
esac

# The session bus.
eval "$(dbus-launch --sh-syntax)"
echo "$DBUS_SESSION_BUS_PID" >> "$folder/pids"

if [ "$desktop" = sway ]; then
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
    # Headless: one virtual output, software rendering (pixman), no input devices. --unsupported-gpu: sway refuses
    # to start where NVIDIA's driver is loaded, though the headless backend and pixman never use the GPU.
    WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
        setsid sway --unsupported-gpu --config "$folder/config/sway/config" > "$folder/log/compositor.log" 2>&1 \
        < /dev/null &
elif [ "$desktop" = gnome ]; then
    # Headless on a virtual monitor; mutter renders on the CPU where there is no GPU it can use.
    setsid mutter --headless --wayland --virtual-monitor "$size" > "$folder/log/compositor.log" 2>&1 < /dev/null &
else
    # The virtual backend: one output, no input devices, software rendering. Without XWayland: it crashed there
    # (KWin 5.27 in the Ubuntu 24.04 root of scripts/noble_root_linux.sh), and the tests' programs are Wayland ones.
    setsid kwin_wayland --virtual --width "${size%x*}" --height "${size#*x}" --no-lockscreen \
        > "$folder/log/compositor.log" 2>&1 < /dev/null &
fi
echo $! >> "$folder/pids"
for _ in $(seq 100); do
    display=$(cd "$XDG_RUNTIME_DIR" && ls wayland-* 2> /dev/null | grep -v '\.lock$' | head -1)
    [ -n "$display" ] && break
    sleep 0.1
done
if [ -z "${display:-}" ]; then
    tail -20 "$folder/log/compositor.log"
    echo "WAYLAND_FAIL $desktop"
    stop
    exit 1
fi
export WAYLAND_DISPLAY=$display
sleep 1
swaysock=''
if [ "$desktop" = sway ]; then
    swaysock=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock 2> /dev/null | head -1)
    export SWAYSOCK=$swaysock
    # XWayland's display, as sway gives it to what it starts.
    swaymsg exec "echo \$DISPLAY > $folder/x11-display" > /dev/null
    sleep 0.5
    x11=$(cat "$folder/x11-display" 2> /dev/null)
elif [ "$desktop" = gnome ]; then
    x11=$(grep -o 'Using public X11 display :[0-9]*' "$folder/log/compositor.log" | tail -1 | grep -o ':[0-9]*')
else
    # KDE here runs without XWayland (above).
    x11=''
fi
dbus-update-activation-environment WAYLAND_DISPLAY DISPLAY="$x11" XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR XDG_CONFIG_HOME \
    XDG_SESSION_TYPE 2> /dev/null

# PipeWire and its session manager; then the portals (started here rather than by D-Bus, so they get this
# environment and their logs land in the folder).
# QUEQIAO_PIPEWIRE_PRELOAD: a library for PipeWire alone (scripts/noble_root_linux.sh: its sandbox check under proot).
setsid env ${QUEQIAO_PIPEWIRE_PRELOAD:+LD_PRELOAD=$QUEQIAO_PIPEWIRE_PRELOAD} pipewire > "$folder/log/pipewire.log" 2>&1 \
    < /dev/null &
echo $! >> "$folder/pids"
sleep 0.5
setsid wireplumber > "$folder/log/wireplumber.log" 2>&1 < /dev/null &
echo $! >> "$folder/pids"
sleep 0.5
if [ "$desktop" = sway ]; then
    setsid /usr/libexec/xdg-desktop-portal-wlr -l DEBUG > "$folder/log/portal-backend.log" 2>&1 < /dev/null &
elif [ "$desktop" = gnome ]; then
    DISPLAY=$x11 setsid /usr/libexec/xdg-desktop-portal-gnome -v > "$folder/log/portal-backend.log" 2>&1 < /dev/null &
else
    kde_portal=$(ls /usr/libexec/xdg-desktop-portal-kde /usr/lib/*/libexec/xdg-desktop-portal-kde 2> /dev/null | head -1)
    DISPLAY=$x11 setsid "$kde_portal" > "$folder/log/portal-backend.log" 2>&1 < /dev/null &
fi
echo $! >> "$folder/pids"
sleep 0.5
setsid /usr/libexec/xdg-desktop-portal -v > "$folder/log/portal.log" 2>&1 < /dev/null &
echo $! >> "$folder/pids"
sleep 1

cat > "$folder/env" << EOF
export XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR
export XDG_CONFIG_HOME=$XDG_CONFIG_HOME
export XDG_CURRENT_DESKTOP=$XDG_CURRENT_DESKTOP
export XDG_SESSION_TYPE=wayland
export WAYLAND_DISPLAY=$WAYLAND_DISPLAY
export SWAYSOCK=$swaysock
export DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS
export DISPLAY=$x11
export QUEQIAO_DESKTOP=$desktop
EOF
cat "$folder/env"
echo "WAYLAND_RUNNING $WAYLAND_DISPLAY $desktop"
