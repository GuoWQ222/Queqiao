#!/bin/bash
# Queqiao's Linux packages (docs/LINUX.md): a .deb (Ubuntu, Debian) and an .rpm (Fedora, openSUSE, RHEL), from one
# release build and packaging/linux (files.txt says what goes where). On a Linux build machine with Rust and dpkg-deb;
# the .rpm needs rpmbuild (on Ubuntu: the rpm package).
# Usage: scripts/package_linux.sh [--test] [--version x.y.z] [--out <folder>]
#   --test     the tests' build (the test-switches features: the options the service's test settings pass to its
#              agents); never published
#   --version  the packages' and the programs' version (QUEQIAO_VERSION; default the workspace's)
#   --out      where the packages go (default $CARGO_TARGET_DIR/packages)
# Environment: CARGO_ARGS (more for cargo build, e.g. --offline).
# Prints PACKAGE_DEB <file>, PACKAGE_RPM <file>, then PACKAGE_DONE, or PACKAGE_FAIL <step>.
set -u
ws=$(cd "$(dirname "$0")/.." && pwd)
target=${CARGO_TARGET_DIR:-$ws/target}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ws/Cargo.toml" | head -1)
test=0
out=$target/packages
while [ $# -gt 0 ]; do
    case "$1" in
        --test) test=1 ;;
        --version) version=$2; shift ;;
        --out) out=$2; shift ;;
        *) echo "PACKAGE_FAIL unknown option $1"; exit 2 ;;
    esac
    shift
done
fail() { echo "PACKAGE_FAIL $1"; exit 1; }
echo "$version" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$' || fail "version $version is not x.y.z"

features=()
[ $test = 1 ] && features=(--features "queqiao-agent/test-switches,queqiao-viewer/test-switches,queqiao-service/test-switches")
build_dir=$target
if [ $test = 0 ]; then
    # A shipping build: nothing of the build machine in the programs. Rust keeps each library's source path for its
    # error messages (/root/.cargo/registry/src/<mirror>/..., the builder's folders: the security review of
    # 2026-11-26): rewritten to cargo/..., rustup/..., queqiao/..., target/.... In a folder of its own (as Windows'
    # target\ship): with other flags than the tests' builds, a shared one would build everything again each time.
    cargo_home=${CARGO_HOME:-$HOME/.cargo}
    rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
    build_dir=$target/ship
    sep=$(printf '\037')
    export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$cargo_home=cargo$sep--remap-path-prefix=$rustup_home=rustup$sep--remap-path-prefix=$build_dir=target$sep--remap-path-prefix=$ws=queqiao"
    # The C and C++ libraries keep their files' paths too (libopus' checks): the compiler rewrites them likewise.
    prefix_maps="-ffile-prefix-map=$cargo_home=cargo -ffile-prefix-map=$build_dir=target -ffile-prefix-map=$ws=queqiao"
    export CFLAGS="${CFLAGS:-} $prefix_maps" CXXFLAGS="${CXXFLAGS:-} $prefix_maps"
fi
# shellcheck disable=SC2086
(cd "$ws" && CARGO_TARGET_DIR=$build_dir QUEQIAO_VERSION=$version cargo build --release -q ${CARGO_ARGS:-} \
    -p queqiao-agent -p queqiao-viewer -p queqiao-service "${features[@]}") || fail build
unset CARGO_ENCODED_RUSTFLAGS CFLAGS CXXFLAGS
bin=$build_dir/release

mkdir -p "$out" || fail "out folder"
stage=$out/stage-$version
rm -rf "$stage" && mkdir -p "$stage" || fail stage

# The icons: each image of assets/queqiao.ico (the one Windows embeds) as hicolor/<n>x<n>/apps/queqiao.png. PNG images
# are taken as they are; 32-bit bitmaps (the small ones) are written out as PNG (Python's standard library only).
icons() {
    python3 - "$ws/assets/queqiao.ico" "$1" << 'PY'
import os, struct, sys, zlib
data = open(sys.argv[1], 'rb').read()
count = struct.unpack_from('<H', data, 4)[0]
for i in range(count):
    w, h, _, _, _, _, length, offset = struct.unpack_from('<BBBBHHII', data, 6 + 16 * i)
    w, h = w or 256, h or 256
    image = data[offset:offset + length]
    folder = os.path.join(sys.argv[2], f'{w}x{h}', 'apps')
    os.makedirs(folder, exist_ok=True)
    path = os.path.join(folder, 'queqiao.png')
    if image.startswith(b'\x89PNG'):
        open(path, 'wb').write(image)
        continue
    header = struct.unpack_from('<IiiHH', image, 0)
    if header[4] != 32:
        continue
    rows = []
    stride = w * 4
    start = header[0]
    for y in range(h):
        row = image[start + (h - 1 - y) * stride:start + (h - y) * stride]
        rgba = bytearray()
        for x in range(w):
            b, g, r, a = row[4 * x:4 * x + 4]
            rgba += bytes((r, g, b, a))
        rows.append(b'\0' + bytes(rgba))
    def chunk(kind, body):
        return struct.pack('>I', len(body)) + kind + body + struct.pack('>I', zlib.crc32(kind + body) & 0xffffffff)
    png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 6, 0, 0, 0))
    png += chunk(b'IDAT', zlib.compress(b''.join(rows), 9)) + chunk(b'IEND', b'')
    open(path, 'wb').write(png)
PY
}

while read -r source path mode; do
    case "$source" in ''|'#'*) continue ;; esac
    mkdir -p "$stage$(dirname "$path")"
    case "$source" in
        @queqiao-*)
            install -m "$mode" "$bin/${source#@}" "$stage$path" || fail "copy ${source#@}"
            strip --strip-unneeded "$stage$path" 2> /dev/null || true
            ;;
        @pam) install -m "$mode" "$ws/packaging/linux/pam.debian" "$stage$path" || fail pam ;;
        @icons) icons "$stage$path" || fail icons ;;
        *) install -m "$mode" "$ws/$source" "$stage$path" || fail "copy $source" ;;
    esac
done < "$ws/packaging/linux/files.txt"

# Nothing of the build machine in the programs as they ship (stripped: the symbol table, with the assembler's file
# names, is gone by then).
if [ $test = 0 ]; then
    for program in queqiao-agent queqiao-viewer queqiao-service; do
        for folder in "$HOME" "$cargo_home" "$rustup_home" "$target" "$ws"; do
            if grep -q -a -F -- "$folder/" "$stage/usr/lib/queqiao/$program"; then
                fail "$program holds the build machine's folder $folder"
            fi
        done
    done
    echo "no build machine folders in the programs"
fi

# The .deb.
deb_root=$out/deb-$version
rm -rf "$deb_root" && cp -a "$stage" "$deb_root" && mkdir -p "$deb_root/DEBIAN" || fail "deb tree"
size=$(du -sk --exclude=DEBIAN "$deb_root" | cut -f1)
cat > "$deb_root/DEBIAN/control" << EOF
Package: queqiao
Version: $version
Architecture: amd64
Maintainer: Queqiao <129141646+GuoWQ222@users.noreply.github.com>
Depends: libc6 (>= 2.35), libstdc++6, libgcc-s1, libssl3 | libssl3t64, curl, systemd
Recommends: libpulse0, libpipewire-0.3-0 | libpipewire-0.3-0t64, libxkbcommon0, libegl1, zenity, libnotify-bin, xdg-desktop-portal
Section: net
Priority: optional
Installed-Size: $size
Homepage: https://github.com/GuoWQ222/Queqiao
Description: Queqiao remote control
 Control your own devices, or help someone. The Queqiao service runs Queqiao
 for the person signed in, and lets the account's own devices use the sign-in
 and lock screens when the person allows it.
EOF
echo /etc/pam.d/queqiao > "$deb_root/DEBIAN/conffiles"
for script in postinst prerm postrm; do
    install -m 0755 "$ws/packaging/linux/deb/$script" "$deb_root/DEBIAN/$script" || fail "deb $script"
done
deb=$out/queqiao_${version}_amd64.deb
dpkg-deb --root-owner-group -Zxz --build "$deb_root" "$deb" > /dev/null || fail dpkg-deb
echo "PACKAGE_DEB $deb"

# The .rpm, where rpmbuild is.
if command -v rpmbuild > /dev/null 2>&1; then
    rpm_stage=$out/rpm-stage-$version
    rm -rf "$rpm_stage" && cp -a "$stage" "$rpm_stage" || fail "rpm tree"
    install -m 0644 "$ws/packaging/linux/pam.fedora" "$rpm_stage/etc/pam.d/queqiao" || fail "rpm pam"
    top=$out/rpmbuild
    rm -rf "$top" && mkdir -p "$top/SPECS" || fail rpmbuild
    sed -e "s|@VERSION@|$version|" -e "s|@STAGE@|$rpm_stage|" "$ws/packaging/linux/rpm/queqiao.spec.in" > "$top/SPECS/queqiao.spec"
    rpmbuild -bb --quiet --define "_topdir $top" --define "_rpmdir $out" \
        --define "_build_name_fmt %%{NAME}-%%{VERSION}-%%{RELEASE}.%%{ARCH}.rpm" "$top/SPECS/queqiao.spec" \
        > "$out/rpmbuild.log" 2>&1 || { tail -5 "$out/rpmbuild.log"; fail rpmbuild; }
    echo "PACKAGE_RPM $out/queqiao-$version-1.x86_64.rpm"
else
    echo "PACKAGE_RPM none (no rpmbuild here)"
fi
echo PACKAGE_DONE
