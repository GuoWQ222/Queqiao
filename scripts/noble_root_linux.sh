#!/usr/bin/env bash
# Tests on a newer desktop (docs/LINUX.md): an Ubuntu 24.04 root file system on the Linux build machine (Ubuntu 22.04 in
# a container without mount rights or user namespaces), entered with proot, for GNOME 46: mutter, xdg-desktop-portal
# 1.18 and its GNOME backend, whose RemoteDesktop portal keeps the person's answer (version 2), which 22.04's (1.14)
# does not. The build folder, the tests' output folder and the workspace are bound in at the same paths, so the tests'
# commands run unchanged inside it (scripts/wayland_display_linux.sh with DESKTOP=gnome, the agent, the test window).
# Usage (as root on the Linux build machine):
#   scripts/noble_root_linux.sh setup          the root under $NOBLE_ROOT (/tmp/noble), about 1 GB; once
#   scripts/noble_root_linux.sh run <command>  <command> inside it as the test user (qqtest), through bash
#   scripts/noble_root_linux.sh root <command> the same as root
# Needs: curl, the mirrors below, GitHub (proot 5.5.0: 22.04's proot 5.1 does not know statx, which 24.04's C library
# uses for every stat, so files there were not found).
set -u
root=${NOBLE_ROOT:-/tmp/noble}
ws=$(cd "$(dirname "$0")/.." && pwd)
target=${CARGO_TARGET_DIR:-$ws/target}
out=/tmp/queqiao_wayland_test
base=https://mirrors.aliyun.com/ubuntu-cdimage/ubuntu-base/releases/24.04/release/ubuntu-base-24.04.3-base-amd64.tar.gz
# Aliyun: USTC throttled this node (2026-11-25: a few packages a minute).
archive=http://mirrors.aliyun.com/ubuntu/
proot_url=https://github.com/proot-me/proot/releases/download/v5.5.0/proot
# Its SHA-256 as the release's SHA256SUMS gives it: anything else is refused.
proot_sha256=db185300a9daabbce86aca7632d8c7fdc4249f6f153171954013b70c5f491ec0
proot=$root.proot
packages=(mutter xdg-desktop-portal xdg-desktop-portal-gnome xdg-desktop-portal-gtk pipewire pipewire-bin wireplumber
    dbus dbus-daemon dbus-x11 at-spi2-core python3-pyatspi python3-gi gir1.2-gtk-4.0 xwayland libegl1 libgl1
    libgl1-mesa-dri libglx-mesa0 libxkbcommon0 libxcursor1 libxi6 libxrandr2 libx11-xcb1 libwayland-client0
    libwayland-egl1 libssl3t64 libpulse0 libpipewire-0.3-0t64 fontconfig fonts-dejavu-core gsettings-desktop-schemas
    dconf-gsettings-backend wmctrl x11-utils procps psmisc
    # KDE (Plasma 5.27 on 24.04): KWin headless on its virtual backend, its portal backend (Qt: AT-SPI built in).
    kwin-wayland xdg-desktop-portal-kde qtwayland5)

# proot binds the folders; run as root on the Linux build machine, commands inside run as root for real. -0 (pretended
# root) only for apt, whose dpkg would otherwise trip over proot. Programs started in the background keep their proot
# running: start them apart (setsid ... &).
inside() {
    mkdir -p "$out" "$target"
    chmod 777 "$out"
    ${inside_exec:+exec} "$proot" -r "$root" -b /proc -b /dev -b /sys -b /dev/shm -b "$target" -b "$out" -b "$ws" -w / "$@"
}

case "${1:-}" in
    setup)
        if ! echo "$proot_sha256  $proot" | sha256sum -c --status 2> /dev/null; then
            curl -sSL -o "$proot.new" "$proot_url" || { echo NOBLE_FAIL proot download; exit 1; }
            echo "$proot_sha256  $proot.new" | sha256sum -c --status || { echo NOBLE_FAIL proot checksum; exit 1; }
            mv -f "$proot.new" "$proot" && chmod 755 "$proot"
        fi
        if ! [ -x "$root/usr/bin/apt-get" ]; then
            mkdir -p "$root"
            [ -s "$root.tar.gz" ] || curl -s -o "$root.tar.gz" "$base" || { echo NOBLE_FAIL download; exit 1; }
            tar -xzf "$root.tar.gz" -C "$root" || { echo NOBLE_FAIL unpack; exit 1; }
        fi
        if ! [ -x "$root/usr/bin/mutter" ] || ! [ -x "$root/usr/bin/kwin_wayland" ]; then
            cp /etc/resolv.conf "$root/etc/resolv.conf"
            cat > "$root/etc/apt/sources.list.d/ubuntu.sources" << EOF
Types: deb
URIs: $archive
Suites: noble noble-updates noble-security
Components: main restricted universe
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
EOF
            # apt's download sandbox (the _apt user) does not work under proot; no package may start a service here
            # (x11-common's would fail, and every X11 package after it).
            echo 'APT::Sandbox::User "root";' > "$root/etc/apt/apt.conf.d/99proot"
            printf '#!/bin/sh\nexit 101\n' > "$root/usr/sbin/policy-rc.d"
            chmod 755 "$root/usr/sbin/policy-rc.d"
            apt_in() {
                inside -0 /usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin HOME=/root DEBIAN_FRONTEND=noninteractive \
                    apt-get -q "$@"
            }
            apt_in update > "$root.apt.log" 2>&1 || { tail -5 "$root.apt.log"; echo NOBLE_FAIL apt-update; exit 1; }
            apt_in install -y --no-install-recommends "${packages[@]}" >> "$root.apt.log" 2>&1 \
                || { tail -5 "$root.apt.log"; echo NOBLE_FAIL apt-install; exit 1; }
        fi
        # The test user, with the same number as outside (files in the bound folders stay theirs).
        uid=$(id -u qqtest 2> /dev/null || echo 1001)
        inside /bin/sh -c "id qqtest > /dev/null 2>&1 || useradd -m -u $uid -s /bin/bash qqtest"
        # What a booted system has: XWayland's socket folders (mutter stops without them) and /run/dbus.
        inside /bin/sh -c 'mkdir -p /run/dbus /tmp/.X11-unix /tmp/.ICE-unix && chmod 1777 /tmp /tmp/.X11-unix /tmp/.ICE-unix'
        # A system bus that does not change user (dbus-daemon's own config makes it messagebus, and proot cannot follow
        # a process that changes user): `scripts/noble_root_linux.sh root "exec dbus-daemon --config-file=..."`.
        cat > "$root/etc/dbus-1/queqiao-system.conf" << 'EOF'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>system</type>
  <listen>unix:path=/run/dbus/system_bus_socket</listen>
  <auth>EXTERNAL</auth>
  <servicedir>/usr/share/dbus-1/system-services</servicedir>
  <policy context="default">
    <allow user="*"/>
    <allow own="*"/>
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
  </policy>
</busconfig>
EOF
        # PipeWire under proot: its access module asks whether each client is sandboxed (Flatpak) by opening
        # /proc/<pid>/root, which proot cannot open for a process it does not trace ("no such file"); PipeWire takes a
        # failed check for a sandboxed client and holds it back (WirePlumber, the session manager, gives up). An
        # answer "permission denied" is what PipeWire reads as "cannot tell: not sandboxed". Loaded into PipeWire only
        # (QUEQIAO_PIPEWIRE_PRELOAD for scripts/wayland_display_linux.sh); nothing is sandboxed in this root.
        rm -f "$root/etc/pipewire/pipewire.conf"
        mkdir -p "$root/usr/local/lib"
        cat > "$root.shim.c" << 'EOF'
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <string.h>

typedef int (*openat_fn)(int, const char *, int, ...);

/* /proc/<pid>/root: "permission denied"; anything else goes through. */
static int open_through(const char *name, openat_fn *real, int dirfd, const char *path, int flags, mode_t mode) {
    if (path != NULL && strncmp(path, "/proc/", 6) == 0) {
        const char *end = strrchr(path, '/');
        if (end != NULL && end != path + 5 && strcmp(end, "/root") == 0) {
            errno = EACCES;
            return -1;
        }
    }
    if (*real == NULL) {
        *real = (openat_fn)dlsym(RTLD_NEXT, name);
    }
    return (*real)(dirfd, path, flags, mode);
}

/* PipeWire calls openat64 (64-bit file offsets); openat as well, for whatever else does. */
int openat64(int dirfd, const char *path, int flags, ...) {
    static openat_fn real;
    mode_t mode = 0;
    if (flags & (O_CREAT | O_TMPFILE)) {
        va_list args;
        va_start(args, flags);
        mode = va_arg(args, mode_t);
        va_end(args);
    }
    return open_through("openat64", &real, dirfd, path, flags, mode);
}

int openat(int dirfd, const char *path, int flags, ...) {
    static openat_fn real;
    mode_t mode = 0;
    if (flags & (O_CREAT | O_TMPFILE)) {
        va_list args;
        va_start(args, flags);
        mode = va_arg(args, mode_t);
        va_end(args);
    }
    return open_through("openat", &real, dirfd, path, flags, mode);
}
EOF
        gcc -shared -fPIC -O2 -o "$root/usr/local/lib/queqiao-proot-shim.so" "$root.shim.c" -ldl \
            || { echo NOBLE_FAIL shim; exit 1; }
        echo "NOBLE_READY $(inside /bin/sh -c '. /etc/os-release; echo $VERSION_ID') mutter $(inside mutter --version 2> /dev/null | head -1 | grep -o '[0-9][0-9.]*')"
        ;;
    run)
        shift
        # The test user from the start: a process that changes user inside proot becomes non-dumpable, and proot can
        # no longer write into it in this container (its next exec fails, "Bad address"). The workspace is not bound
        # (the user may not read it): what the user runs is copied into the output folder.
        mkdir -p "$out"
        chmod 777 "$out"
        # From / (the user may not enter the folder this was started in).
        cd / && exec runuser -u qqtest -- "$proot" -r "$root" -b /proc -b /dev -b /sys -b /dev/shm -b "$target" -b "$out" -w / \
            /usr/bin/env -i PATH=/usr/local/bin:/usr/bin:/bin HOME=/home/qqtest USER=qqtest LANG=C.UTF-8 \
            /bin/bash -c "$*"
        ;;
    root)
        shift
        # From /, and in place of this script: what runs long (the system bus) holds no file of the workspace.
        cd / && inside_exec=1 inside /usr/bin/env -i PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin HOME=/root \
            LANG=C.UTF-8 /bin/bash -c "$*"
        ;;
    *) echo "usage: $0 setup | run <command> | root <command>"; exit 2 ;;
esac
