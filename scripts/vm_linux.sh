#!/bin/bash
# A test machine for what a container cannot have (docs/LINUX.md, "Where it is tested"): systemd and logind, a
# sign-in screen (GDM), the lock screen, /dev/uinput, KMS (virtio-gpu) and package installs with their scripts.
# QEMU boots Ubuntu 24.04 with GNOME (ubuntu-desktop-minimal and GDM) from a disk built here out of Ubuntu's packages:
# Ubuntu Base and apt inside a root folder entered with proot (as scripts/noble_root_linux.sh does), written into an
# ext4 image by mke2fs -d (no mount needed), booted with its own kernel and initramfs. Without /dev/kvm (the Linux build
# machine is a container) QEMU emulates the CPU (TCG, many threads): slow but complete. Only this host reaches it (SSH
# on 127.0.0.1:$SSH_PORT).
#
# Usage:
#   scripts/vm_linux.sh setup          builds the disk (about 20 minutes, once)
#   scripts/vm_linux.sh start          boots it (daemon); `wait` waits until SSH answers
#   scripts/vm_linux.sh ssh <command>  runs a command in it as user qq (passwordless sudo)
#   scripts/vm_linux.sh put <file> <path>, get <path> <file>
#   scripts/vm_linux.sh screen <file.png>   what its screen shows (QMP screendump)
#   scripts/vm_linux.sh keys <text>    types text on its keyboard as the person at it would (QMP, a USB keyboard)
#   scripts/vm_linux.sh key <qcode>... presses keys together (QMP send-key: ret, ctrl alt delete ...)
#   scripts/vm_linux.sh stop | status
# Environment: VM_DIR (default /tmp/queqiao_vm), APT_MIRROR (Aliyun's internal one: the Linux build machine runs in
# Aliyun; elsewhere archive.ubuntu.com or a mirror near it), SSH_PORT (2222),
# CPUS (16), MEMORY (8G).
set -u
VM_DIR=${VM_DIR:-/tmp/queqiao_vm}
APT_MIRROR=${APT_MIRROR:-http://mirrors.cloud.aliyuncs.com/ubuntu/}
BASE=${BASE:-https://mirrors.aliyun.com/ubuntu-cdimage/ubuntu-base/releases/24.04/release/ubuntu-base-24.04.3-base-amd64.tar.gz}
# proot 5.5.0 as scripts/noble_root_linux.sh checks it (the same file, kept beside that root when it is there).
PROOT_URL=https://github.com/proot-me/proot/releases/download/v5.5.0/proot
PROOT_SHA256=db185300a9daabbce86aca7632d8c7fdc4249f6f153171954013b70c5f491ec0
SSH_PORT=${SSH_PORT:-2222}
CPUS=${CPUS:-16}
MEMORY=${MEMORY:-8G}
# The test account in the machine (only this host reaches the machine).
VM_USER=qq
VM_PASSWORD=qq-test-1
SSH_OPTS=(-i "$VM_DIR/key" -p "$SSH_PORT" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10 -o BatchMode=yes)

qmp() {
    # One QMP command (JSON) on the machine's monitor socket; prints the reply.
    python3 - "$VM_DIR/qmp.sock" "$1" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
f = s.makefile('rw')
f.readline()
f.write(json.dumps({"execute": "qmp_capabilities"}) + "\n"); f.flush(); f.readline()
f.write(sys.argv[2] + "\n"); f.flush()
while True:
    line = f.readline()
    if not line:
        break
    reply = json.loads(line)
    if "return" in reply or "error" in reply:
        print(json.dumps(reply))
        break
PY
}

boot() {
    qemu-system-x86_64 -name queqiao-vm -accel tcg,thread=multi -cpu max -smp "$CPUS" -m "$MEMORY" \
        -kernel "$VM_DIR/vmlinuz" -initrd "$VM_DIR/initrd.img" -append "root=LABEL=queqiao-root rw console=ttyS0" \
        -drive "file=$VM_DIR/disk.qcow2,if=virtio,format=qcow2" \
        -device virtio-vga -display none \
        -device qemu-xhci -device usb-kbd -device usb-tablet \
        -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:$SSH_PORT-:22" -device virtio-net-pci,netdev=net0 \
        -qmp "unix:$VM_DIR/qmp.sock,server=on,wait=off" -serial "file:$VM_DIR/serial.log" \
        -pidfile "$VM_DIR/qemu.pid" -daemonize
}

case "${1:-}" in
setup)
    mkdir -p "$VM_DIR" && cd "$VM_DIR" || exit 1
    root=$VM_DIR/root
    proot=$VM_DIR/proot
    [ -x "$proot" ] || cp /tmp/noble.proot "$proot" 2> /dev/null || curl -sSL -o "$proot" "$PROOT_URL"
    echo "$PROOT_SHA256  $proot" | sha256sum -c --status || { rm -f "$proot"; echo "VM_FAIL proot checksum"; exit 1; }
    chmod 755 "$proot"
    if ! [ -x "$root/usr/bin/apt-get" ]; then
        rm -rf "$root" && mkdir -p "$root"
        [ -s base.tar.gz ] || cp /tmp/noble.tar.gz base.tar.gz 2> /dev/null || curl -fsS -o base.tar.gz "$BASE" || { echo "VM_FAIL download"; exit 1; }
        tar -xzf base.tar.gz -C "$root" || { echo "VM_FAIL unpack"; exit 1; }
    fi
    cp /etc/resolv.conf "$root/etc/resolv.conf"
    cat > "$root/etc/apt/sources.list.d/ubuntu.sources" << EOF
Types: deb
URIs: $APT_MIRROR
Suites: noble noble-updates noble-security
Components: main restricted universe
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
EOF
    # While it is built here: apt's sandbox user does not work under proot, and no service may start.
    echo 'APT::Sandbox::User "root";' > "$root/etc/apt/apt.conf.d/99proot"
    printf '#!/bin/sh\nexit 101\n' > "$root/usr/sbin/policy-rc.d" && chmod 755 "$root/usr/sbin/policy-rc.d"
    inside() { "$proot" -0 -r "$root" -b /proc -b /dev -b /sys -w / /usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin HOME=/root DEBIAN_FRONTEND=noninteractive "$@"; }
    packages=(linux-image-generic systemd-sysv udev dbus-user-session libpam-systemd openssh-server sudo locales
        ubuntu-desktop-minimal gdm3 gnome-shell gnome-session ubuntu-session yaru-theme-gnome-shell yaru-theme-gtk
        yaru-theme-icon fonts-ubuntu fonts-dejavu-core network-manager gnome-terminal xdg-desktop-portal-gnome
        pipewire wireplumber pipewire-pulse xwayland python3-pil x11-utils wmctrl less initramfs-tools kmod
        systemd-resolved curl)
    inside apt-get -q update > apt.log 2>&1 || { tail -5 apt.log; echo "VM_FAIL apt update"; exit 1; }
    inside apt-get -q install -y --no-install-recommends "${packages[@]}" >> apt.log 2>&1 || { tail -8 apt.log; echo "VM_FAIL apt install"; exit 1; }
    inside apt-get -q clean >> apt.log 2>&1
    # The system users systemd's packages declare (their scripts could not make them here), and the initramfs
    # (initramfs-tools came after the kernel).
    inside systemd-sysusers >> apt.log 2>&1
    ls "$root"/boot/initrd.img-* > /dev/null 2>&1 || inside update-initramfs -c -k all >> apt.log 2>&1
    ls "$root"/boot/vmlinuz-* "$root"/boot/initrd.img-* > /dev/null 2>&1 || { echo "VM_FAIL no kernel or initramfs"; exit 1; }
    # The machine as a booted system needs it: the test user, SSH with this host's key, the disk, the network (the
    # emulated card, DHCP from QEMU; NetworkManager leaves wired cards alone on Ubuntu unless told), its name.
    [ -s key ] || ssh-keygen -q -t ed25519 -N '' -f key
    inside /bin/sh -c "id $VM_USER > /dev/null 2>&1 || useradd -m -s /bin/bash -G sudo,adm $VM_USER; echo '$VM_USER:$VM_PASSWORD' | chpasswd"
    echo "$VM_USER ALL=(ALL) NOPASSWD:ALL" > "$root/etc/sudoers.d/90-$VM_USER" && chmod 440 "$root/etc/sudoers.d/90-$VM_USER"
    home=$root/home/$VM_USER
    mkdir -p "$home/.ssh" && cp key.pub "$home/.ssh/authorized_keys" && chmod 700 "$home/.ssh" && chmod 600 "$home/.ssh/authorized_keys"
    inside chown -R "$VM_USER:$VM_USER" "/home/$VM_USER"
    echo 'LABEL=queqiao-root / ext4 defaults 0 1' > "$root/etc/fstab"
    echo queqiao-vm > "$root/etc/hostname"
    printf '127.0.0.1 localhost\n127.0.1.1 queqiao-vm\n' > "$root/etc/hosts"
    : > "$root/etc/machine-id"
    : > "$root/etc/NetworkManager/conf.d/10-globally-managed-devices.conf"
    # QEMU's DNS (it forwards to this host's), for systemd-resolved whatever NetworkManager tells it.
    mkdir -p "$root/etc/systemd/resolved.conf.d"
    printf '[Resolve]\nDNS=10.0.2.3\n' > "$root/etc/systemd/resolved.conf.d/qemu.conf"
    rm -f "$root/etc/resolv.conf" && ln -s ../run/systemd/resolve/stub-resolv.conf "$root/etc/resolv.conf"
    [ -x "$root/usr/lib/systemd/systemd-resolved" ] || { rm -f "$root/etc/resolv.conf"; echo 'nameserver 10.0.2.3' > "$root/etc/resolv.conf"; }
    inside systemctl set-default graphical.target >> apt.log 2>&1
    inside systemctl enable ssh gdm3 NetworkManager systemd-resolved >> apt.log 2>&1
    echo 'en_US.UTF-8 UTF-8' > "$root/etc/locale.gen" && inside locale-gen >> apt.log 2>&1
    echo 'LANG=en_US.UTF-8' > "$root/etc/default/locale"
    rm -f "$root/usr/sbin/policy-rc.d" "$root/etc/apt/apt.conf.d/99proot"
    cp -L "$(ls "$root"/boot/vmlinuz-* | sort -V | tail -1)" vmlinuz
    cp -L "$(ls "$root"/boot/initrd.img-* | sort -V | tail -1)" initrd.img
    rm -f disk.raw disk.qcow2
    mke2fs -q -t ext4 -L queqiao-root -d "$root" disk.raw 40G || { echo "VM_FAIL mke2fs"; exit 1; }
    qemu-img convert -O qcow2 disk.raw disk.qcow2 && rm -f disk.raw || { echo "VM_FAIL qcow2"; exit 1; }
    echo VM_SETUP_DONE
    ;;
start)
    cd "$VM_DIR" || exit 1
    kill -0 "$(cat qemu.pid 2>/dev/null)" 2>/dev/null && { echo "VM_RUNNING"; exit 0; }
    : > serial.log
    boot "" && echo VM_STARTED
    ;;
wait)
    for _ in $(seq 1 120); do
        ssh "${SSH_OPTS[@]}" "$VM_USER@127.0.0.1" true 2>/dev/null && { echo VM_UP; exit 0; }
        sleep 5
    done
    echo "VM_FAIL no SSH after 10 min"; exit 1
    ;;
ssh)
    shift
    exec ssh "${SSH_OPTS[@]}" "$VM_USER@127.0.0.1" "$@"
    ;;
put)
    exec scp -q -i "$VM_DIR/key" -P "$SSH_PORT" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR "$2" "$VM_USER@127.0.0.1:$3"
    ;;
get)
    exec scp -q -i "$VM_DIR/key" -P "$SSH_PORT" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR "$VM_USER@127.0.0.1:$2" "$3"
    ;;
screen)
    out=$2
    qmp "{\"execute\": \"screendump\", \"arguments\": {\"filename\": \"$VM_DIR/screen.ppm\"}}" > /dev/null
    python3 -c "from PIL import Image; import sys; Image.open(sys.argv[1]).save(sys.argv[2])" "$VM_DIR/screen.ppm" "$out" && echo "SCREEN $out"
    ;;
keys)
    # Each character as the US keyboard types it (letters, digits, space and a few signs).
    python3 - "$2" <<'PY' | while read -r json; do qmp "$json" > /dev/null; sleep 0.15; done
import json, sys
shifted = {'!': '1', '@': '2', '#': '3', '$': '4', '%': '5', '^': '6', '&': '7', '*': '8', '(': '9', ')': '0',
           '_': 'minus', '+': 'equal', ':': 'semicolon', '"': 'apostrophe', '?': 'slash', '<': 'comma', '>': 'dot'}
plain = {' ': 'spc', '-': 'minus', '=': 'equal', '.': 'dot', ',': 'comma', '/': 'slash', ';': 'semicolon', "'": 'apostrophe'}
for c in sys.argv[1]:
    if c.isalpha() or c.isdigit():
        keys = (['shift'] if c.isupper() else []) + [c.lower()]
    elif c in shifted:
        keys = ['shift', shifted[c]]
    else:
        keys = [plain[c]]
    print(json.dumps({"execute": "send-key", "arguments": {"keys": [{"type": "qcode", "data": k} for k in keys]}}))
PY
    echo KEYS_SENT
    ;;
key)
    shift
    list=$(printf '{"type": "qcode", "data": "%s"},' "$@")
    qmp "{\"execute\": \"send-key\", \"arguments\": {\"keys\": [${list%,}]}}"
    ;;
stop)
    cd "$VM_DIR" 2>/dev/null || exit 0
    if kill -0 "$(cat qemu.pid 2>/dev/null)" 2>/dev/null; then
        qmp '{"execute": "system_powerdown"}' > /dev/null
        for _ in $(seq 1 24); do kill -0 "$(cat qemu.pid)" 2>/dev/null || break; sleep 5; done
        kill "$(cat qemu.pid)" 2>/dev/null
    fi
    echo VM_STOPPED
    ;;
status)
    kill -0 "$(cat "$VM_DIR/qemu.pid" 2>/dev/null)" 2>/dev/null && echo VM_RUNNING || echo VM_STOPPED
    ;;
*)
    sed -n '2,20p' "$0"; exit 2
    ;;
esac
