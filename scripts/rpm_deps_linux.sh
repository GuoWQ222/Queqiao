#!/bin/bash
# The .rpm in Fedora (docs/LINUX.md): its dependencies resolved by Fedora's own dnf, from Fedora's repositories, then
# installed and removed again. A Fedora root (the official container image) entered with proot, as
# scripts/noble_root_linux.sh does for Ubuntu 24.04; its systemd does not run there (systemctl ignores what the
# package's scripts ask), so the service itself is tested by scripts/run_service_vm_linux.sh (rpm step).
# Usage: scripts/rpm_deps_linux.sh <package.rpm>. Environment: FEDORA (43), MIRROR (Aliyun's internal one: cc4-1 runs
# in Aliyun; elsewhere a Fedora mirror near you), FEDORA_ROOT (/tmp/fedora). Ends with RPM_DEPS_PASS or RPM_DEPS_FAIL.
set -u
package=${1:?usage: scripts/rpm_deps_linux.sh <package.rpm>}
fedora=${FEDORA:-43}
mirror=${MIRROR:-http://mirrors.cloud.aliyuncs.com/fedora}
root=${FEDORA_ROOT:-/tmp/fedora}
proot=/tmp/queqiao_vm/proot
fail() { echo "RPM_DEPS_FAIL $1"; exit 1; }
[ -x "$proot" ] || fail "no proot at $proot (scripts/vm_linux.sh setup puts it there)"
if ! [ -x "$root/usr/bin/dnf" ] && ! [ -x "$root/usr/bin/dnf5" ]; then
    images=$mirror/releases/$fedora/Container/x86_64/images/
    name=$(curl -fsS "$images" | grep -o 'Fedora-Container-Base-Generic-[0-9.-]*x86_64\.oci\.tar\.xz' | grep -v Minimal | head -1)
    [ -n "$name" ] || fail "no container image at $images"
    work=$root.oci
    rm -rf "$work" "$root" && mkdir -p "$work" "$root"
    curl -fsS -o "$work/image.tar.xz" "$images$name" || fail download
    tar -xJf "$work/image.tar.xz" -C "$work" || fail "unpack image"
    # The image's one layer: index -> manifest -> layers[0] (a tar, gzip or plain).
    manifest=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['manifests'][0]['digest'].split(':')[1])" "$work/index.json")
    layer=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['layers'][0]['digest'].split(':')[1])" "$work/blobs/sha256/$manifest")
    tar -xf "$work/blobs/sha256/$layer" -C "$root" 2> /dev/null || fail "unpack layer"
    rm -rf "$work"
fi
cp /etc/resolv.conf "$root/etc/resolv.conf"
# Fedora's repositories from the mirror (the metalink would pick one far away).
for repo in "$root"/etc/yum.repos.d/fedora.repo "$root"/etc/yum.repos.d/fedora-updates.repo; do
    [ -f "$repo" ] || continue
    sed -i -e 's|^metalink=|#metalink=|' \
        -e "s|^#baseurl=http://download.example/pub/fedora/linux|baseurl=$mirror|" "$repo"
done
cp "$package" "$root/tmp/queqiao.rpm" || fail "copy package"
inside() { "$proot" -0 -r "$root" -b /proc -b /dev -b /sys -w / /usr/bin/env -i PATH=/usr/sbin:/usr/bin HOME=/root "$@"; }
dnf=dnf
[ -x "$root/usr/bin/dnf5" ] && dnf=dnf5
inside "$dnf" -y -q --setopt=install_weak_deps=False install /tmp/queqiao.rpm > "$root.install.log" 2>&1 \
    || { tail -8 "$root.install.log"; fail "dnf install"; }
inside rpm -q queqiao || fail "not installed"
echo "requires: $(inside rpm -q --requires queqiao | tr '\n' ' ')"
for file in /usr/lib/queqiao/queqiao-agent /usr/lib/queqiao/queqiao-service /etc/pam.d/queqiao \
    /usr/lib/systemd/system/queqiao.service; do
    [ -e "$root$file" ] || fail "$file missing"
done
grep -q system-auth "$root/etc/pam.d/queqiao" || fail "the PAM file is not Fedora's"
inside /usr/lib/queqiao/queqiao-service --version || fail "the service does not run in Fedora (its libraries)"
inside /usr/lib/queqiao/queqiao-agent --version || fail "the agent does not run in Fedora (its libraries)"
inside "$dnf" -y -q remove queqiao > "$root.remove.log" 2>&1 || { tail -5 "$root.remove.log"; fail "dnf remove"; }
[ -e "$root/usr/lib/queqiao" ] && fail "files left after dnf remove"
echo RPM_DEPS_PASS
