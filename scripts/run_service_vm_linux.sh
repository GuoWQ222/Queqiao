#!/bin/bash
# The Queqiao service on a real Linux system (docs/LINUX.md, "Where it is tested"): the test machine of
# scripts/vm_linux.sh (Ubuntu 24.04, GNOME and GDM on Wayland, systemd, uinput, KMS), the test .deb installed there, a
# queqiao-server and the viewer on this host (the machine reaches this host as 10.0.2.2). Steps, each ending in a
# STEP_PASS or STEP_FAIL line:
#   install  the package installs; the service runs; the person's agent runs in qq's session, logged in to the test
#            account, and hands the device over for the sign-in screen
#   lock     qq's session locked: the viewer gives that PC's password (the lock gate; PAM through the service), sees
#            the lock screen through the desktop helper (KMS), types qq's password through it (uinput): unlocked;
#            qq is told afterwards (no notice can show on GNOME's lock screen)
#   signin   qq signed out: the agent for the sign-in screen (system user queqiao) serves the same device; the viewer
#            gives qq's password, sees GDM through the helper and signs qq in by typing; qq's agent is handed the
#            notice the sign-in screen left
#   packages builds the update test's packages into $out/feed-0.1.1 and $out/feed-0.1.2 (0.1.2 broken on purpose: its
#            agent is not a program) and prints each one's name, size and SHA-256 (FEED_PACKAGE lines): the release
#            key signs their manifests where it is (queqiao-setup release-package), into the same folders
#   update   the installed version finds 0.1.1 on the feed (served on this host, reached from the machine through
#            an SSH tunnel as 127.0.0.1, which a test feed must be), downloads it, the service checks and installs it,
#            and it keeps running
#   rollback 0.1.2 on the feed: installed, its agent does not run, the earlier package (0.1.1) is put back
#   purge    apt remove --purge takes everything out
# Usage: scripts/run_service_vm_linux.sh [step...] (default: install lock signin purge, in that order). Needs the
# machine set up once (scripts/vm_linux.sh setup). Ends with SERVICE_VM_PASS or SERVICE_VM_FAIL.
set -u
ws=$(cd "$(dirname "$0")/.." && pwd)
target=${CARGO_TARGET_DIR:-$ws/target}
out=/tmp/queqiao_vm_test
steps=("$@")
[ ${#steps[@]} -eq 0 ] && steps=(install lock signin purge)
vm() { bash "$ws/scripts/vm_linux.sh" "$@"; }
password=qq-test-1
listen=127.0.0.1:47310
failed=0
pass() { echo "STEP_PASS $1"; }
fail() { echo "STEP_FAIL $1: $2"; failed=1; }

mkdir -p "$out"
vm start > /dev/null && vm wait > /dev/null || { echo "SERVICE_VM_FAIL the machine does not start"; exit 1; }

# The test package (test switches: the service passes the test account to its agents), the server and the viewer.
CARGO_ARGS=--offline bash "$ws/scripts/package_linux.sh" --test > "$out/package.txt" 2>&1 \
    || { tail -3 "$out/package.txt"; echo "SERVICE_VM_FAIL package"; exit 1; }
deb=$(sed -n 's/^PACKAGE_DEB //p' "$out/package.txt")
(cd "$ws" && cargo build --release -q --offline -p queqiao-server > "$out/server_build.txt" 2>&1 \
    && cargo build --release -q --offline -p queqiao-input --example uinput_probe >> "$out/server_build.txt" 2>&1) \
    || { echo "SERVICE_VM_FAIL server build"; exit 1; }
vm put "$target/release/examples/uinput_probe" /tmp/uinput_probe
viewer=$target/release/queqiao-viewer

# The server: one account for the machine's agents and the viewer, kept from run to run (the machine's agents stay
# logged in to it between steps run apart); made anew only the first time.
data=$out/server
if ! [ -s "$data/account.txt" ]; then
    pkill -f "queqiao-server serve --listen $listen" 2> /dev/null
    rm -rf "$data" && mkdir -p "$data"
    init=$("$target/release/queqiao-server" init --data "$data" 2>&1)
    pin=$(printf '%s\n' "$init" | grep -o 'fingerprint: [0-9a-f]\{64\}' | head -1 | cut -d' ' -f2)
    account_password=vm-$(date +%s)-$RANDOM
    printf '%s\n' "$account_password" | "$target/release/queqiao-server" add-user alice --data "$data" > /dev/null 2>&1
    printf '%s %s\n' "$pin" "$account_password" > "$data/account.txt"
fi
read -r pin account_password < "$data/account.txt"
if ! pgrep -f "queqiao-server serve --listen $listen" > /dev/null; then
    (setsid nohup "$target/release/queqiao-server" serve --listen "$listen" --data "$data" --public-ip 127.0.0.1 \
        --turn-port 0 < /dev/null >> "$out/server.txt" 2>&1 &)
    sleep 1
fi
login="--server 10.0.2.2:${listen#*:} --pin $pin --user alice --password $account_password"

# The viewer's screen on this host.
pgrep -f "Xvfb :93" > /dev/null || (setsid nohup Xvfb :93 -screen 0 1280x800x24 < /dev/null > /dev/null 2>&1 &)
sleep 1

# What logind says in the machine: qq's session on seat0 (empty when nobody is signed in there).
qq_session() { vm ssh "loginctl list-sessions --no-legend | awk '\$3 == \"qq\" && \$4 == \"seat0\" {print \$1}'" 2> /dev/null; }
# Signs qq in at GDM as the person at the machine would: a click on the user, the password, Enter (a keyboard and
# pointer of the kernel's, made by the probe; QEMU's own keyboard did not reach GDM).
sign_in_locally() {
    vm ssh "sudo /tmp/uinput_probe move:32768,28400 wait:300 click wait:2500 type:$password enter" > /dev/null 2>&1
    for _ in $(seq 1 30); do [ -n "$(qq_session)" ] && return 0; sleep 2; done
    return 1
}
# The update feed and its tunnel, stopped by their own process numbers.
stop_feed() {
    for file in "$out/feed.pid" "$out/tunnel.pid"; do
        [ -s "$file" ] && kill "$(cat "$file")" 2> /dev/null
        rm -f "$file"
    done
}
journal() { vm ssh "sudo journalctl -u queqiao --no-pager -o cat --since '$1'" 2> /dev/null; }

run_viewer() {
    local label=$1
    shift
    mkdir -p "$out/$label"
    DISPLAY=:93 timeout 120 "$viewer" --server "$listen" --pin "$pin" --user alice --password "$account_password" \
        --device queqiao-vm --mute --width 1024 --ui-pictures "$out/$label" --picture-at 2 \
        --report "$out/viewer_$label.txt" "$@" > /dev/null 2>&1
}

for step in "${steps[@]}"; do
    since=$(vm ssh "date '+%Y-%m-%d %H:%M:%S'" 2> /dev/null)
    case "$step" in
    install)
        [ -n "$(qq_session)" ] || sign_in_locally || { fail install "qq could not sign in"; continue; }
        vm put "$deb" /tmp/queqiao.deb
        vm ssh "sudo mkdir -p /etc/queqiao && printf 'agent_arguments=$login --sessions 1 --report /tmp/qq_agent.txt\nlogon_arguments=--sessions 1 --report /var/lib/queqiao-signin/agent.txt\n' | sudo tee /etc/queqiao/service.conf > /dev/null && sudo rm -f /tmp/qq_agent.txt && (sudo dpkg -i /tmp/queqiao.deb || sudo apt-get install -f -y) > /tmp/install.txt 2>&1; tail -2 /tmp/install.txt" > "$out/install.txt" 2>&1
        sleep 25
        status=$(vm ssh "systemctl is-active queqiao; /usr/lib/queqiao/queqiao-service status; id queqiao; ls -l /var/lib/queqiao" 2>&1)
        echo "$status" > "$out/install_status.txt"
        journal "$since" > "$out/install_journal.txt"
        if ! echo "$status" | grep -q '^active'; then
            fail install "the service does not run"
        elif ! grep -q 'started the desktop helper' "$out/install_journal.txt"; then
            fail install "no agent started ($(grep -m1 'not started' "$out/install_journal.txt"))"
        elif ! grep -q 'kept for it' "$out/install_journal.txt"; then
            fail install "the device was not handed over for the sign-in screen"
        else
            pass install
        fi
        ;;
    lock)
        s=$(qq_session)
        [ -n "$s" ] || { fail lock "qq is not signed in"; continue; }
        vm ssh "sudo loginctl lock-session $s"
        sleep 6
        run_viewer lock --seconds 45 --test-password "wrong-one,$password" --type-test "$password"
        sleep 5
        locked=$(vm ssh "loginctl show-session $s -p LockedHint --value" 2> /dev/null)
        journal "$since" > "$out/lock_journal.txt"
        vm ssh "cat /tmp/qq_agent.txt" > "$out/lock_agent.txt" 2>&1
        if ! grep -q 'password for qq: right' "$out/lock_journal.txt"; then
            fail lock "the PC's password was not checked right through the service"
        elif ! grep -q 'pictures and input through the desktop helper' "$out/lock_agent.txt"; then
            fail lock "the agent did not go through the helper"
        elif [ "$locked" != no ]; then
            fail lock "still locked after typing the password (LockedHint=$locked)"
        elif ! grep -q 'notice shown' "$out/lock_agent.txt"; then
            fail lock "qq was not told afterwards"
        else
            pass lock
        fi
        ;;
    signin)
        s=$(qq_session)
        [ -n "$s" ] && vm ssh "sudo loginctl terminate-session $s"
        sleep 12
        run_viewer signin --seconds 60 --test-password "$password" --type-test "$password"
        for _ in $(seq 1 20); do [ -n "$(qq_session)" ] && break; sleep 2; done
        sleep 10
        journal "$since" > "$out/signin_journal.txt"
        vm ssh "sudo cat /var/lib/queqiao-signin/agent.txt" > "$out/signin_agent.txt" 2>&1
        if ! grep -q 'the agent for the sign-in screen' "$out/signin_journal.txt"; then
            fail signin "no agent for the sign-in screen"
        elif ! grep -q 'password for qq: right' "$out/signin_journal.txt"; then
            fail signin "qq's password was not checked right"
        elif [ -z "$(qq_session)" ]; then
            fail signin "qq was not signed in by the viewer's typing"
        elif ! grep -q 'notice handed to qq' "$out/signin_journal.txt"; then
            fail signin "qq's agent was not handed the sign-in screen's notice"
        else
            pass signin
        fi
        ;;
    packages)
        for version in 0.1.1 0.1.2; do
            feed=$out/feed-$version
            rm -rf "$feed" && mkdir -p "$feed"
            CARGO_ARGS=--offline bash "$ws/scripts/package_linux.sh" --test --version "$version" --out "$out/build-$version" \
                > "$out/package_$version.txt" 2>&1 || { fail packages "$version did not build"; continue 2; }
            if [ "$version" = 0.1.2 ]; then
                tree=$out/build-$version/deb-$version
                printf 'not a program\n' > "$tree/usr/lib/queqiao/queqiao-agent"
                dpkg-deb --root-owner-group -Zxz --build "$tree" "$feed/queqiao_${version}_amd64.deb" > /dev/null
            else
                cp "$out/build-$version/queqiao_${version}_amd64.deb" "$feed/"
            fi
            file=$feed/queqiao_${version}_amd64.deb
            echo "FEED_PACKAGE $version $(basename "$file") $(stat -c %s "$file") $(sha256sum "$file" | cut -d' ' -f1)"
        done
        pass packages
        ;;
    update | rollback)
        version=$([ "$step" = update ] && echo 0.1.1 || echo 0.1.2)
        expected=0.1.1
        feed=$out/feed-$version
        [ -s "$feed/queqiao-update-deb.json.sig" ] || { fail "$step" "no signed manifest in $feed"; continue; }
        stop_feed
        # nohup alone: setsid forks when it is a group leader, and $! would be a process that is gone.
        (cd "$feed" || exit; nohup python3 -m http.server 47320 --bind 127.0.0.1 < /dev/null > "$out/feed_$step.txt" 2>&1 & echo $! > "$out/feed.pid")
        # The tunnel: ssh -f leaves it running in the background once it is up (a plain "&" one went when the step's
        # own SSH call ended).
        setsid ssh -f -i /tmp/queqiao_vm/key -p 2222 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
            -o LogLevel=ERROR -o ExitOnForwardFailure=yes -N -R 47320:127.0.0.1:47320 qq@127.0.0.1 \
            < /dev/null > /dev/null 2>&1 || { fail "$step" "no tunnel to the machine"; continue; }
        pgrep -n -f -- "^ssh -f .*-R 47320:127.0.0.1:47320 qq@" > "$out/tunnel.pid"
        sleep 2
        # qq's agent looks at the test feed 5 s after it starts; the service starts it again (a test build reads it),
        # without the test login options (an agent given them never updates): with the login it saved before.
        vm ssh "mkdir -p ~/.config/queqiao && printf 'feed=http://127.0.0.1:47320/\nfirst_check_seconds=5\n' > ~/.config/queqiao/update.conf && printf 'logon_arguments=--sessions 1 --report /var/lib/queqiao-signin/agent.txt\n' | sudo tee /etc/queqiao/service.conf > /dev/null && sudo systemctl restart queqiao"
        result=
        for _ in $(seq 1 60); do
            result=$(vm ssh "sudo journalctl -u 'queqiao-update-*' --no-pager -o cat --since '$since' | grep -E 'installed and keeps running|did not keep running' | tail -1" 2> /dev/null)
            [ -n "$result" ] && break
            sleep 5
        done
        sleep 20
        installed=$(vm ssh "dpkg-query -W -f '\${Version}' queqiao; echo; /usr/lib/queqiao/queqiao-service status | head -1" 2> /dev/null | tr '\n' ' ')
        journal "$since" > "$out/${step}_journal.txt"
        vm ssh "sudo journalctl -u 'queqiao-update-*' --no-pager -o cat --since '$since'" > "$out/${step}_update.txt" 2>&1
        stop_feed
        if [ -z "$result" ]; then
            fail "$step" "no update ran ($(grep -m1 -E 'update|refused' "$out/${step}_journal.txt"))"
        elif [ "$step" = update ] && ! echo "$result" | grep -q "keeps running"; then
            fail update "$result"
        elif [ "$step" = rollback ] && ! grep -q "was put back" "$out/${step}_update.txt"; then
            fail rollback "the earlier package was not put back: $result"
        elif ! echo "$installed" | grep -q "^$expected version=$expected"; then
            fail "$step" "installed now: $installed (expected $expected)"
        else
            pass "$step"
        fi
        ;;
    rpm)
        # The .rpm's own scripts under a real systemd: rpm itself in this Debian-family machine (its database knows
        # no packages, so dependencies are not checked here; a Fedora root checks those: scripts/rpm_deps_linux.sh).
        rpm_file=$(sed -n 's/^PACKAGE_RPM //p' "$out/package.txt")
        [ -s "$rpm_file" ] || { fail rpm "no .rpm built ($rpm_file)"; continue; }
        vm ssh "command -v rpm > /dev/null || sudo apt-get install -y -q rpm > /dev/null 2>&1; dpkg -s queqiao > /dev/null 2>&1 && sudo apt-get remove --purge -y queqiao > /dev/null 2>&1; true"
        vm put "$rpm_file" /tmp/queqiao.rpm
        vm ssh "sudo rpm -i --nodeps /tmp/queqiao.rpm > /tmp/rpm.txt 2>&1; tail -2 /tmp/rpm.txt" > "$out/rpm_install.txt" 2>&1
        sleep 20
        installed=$(vm ssh "systemctl is-active queqiao; /usr/lib/queqiao/queqiao-service status | grep -c '^agent_pid='; id -u queqiao; stat -c %a /var/lib/queqiao-signin; test -c /dev/uinput && echo 1 || echo 0" 2>&1 | tr '\n' ' ')
        vm ssh "sudo rpm -e queqiao > /tmp/rpm_e.txt 2>&1; tail -2 /tmp/rpm_e.txt" > "$out/rpm_remove.txt" 2>&1
        left=$(vm ssh "ls -d /usr/lib/queqiao /var/lib/queqiao /var/lib/queqiao-signin /usr/lib/systemd/system/queqiao.service 2> /dev/null; getent passwd queqiao; systemctl is-active queqiao 2> /dev/null | grep -x active" 2>&1)
        echo "$installed" > "$out/rpm_status.txt"
        if ! echo "$installed" | grep -qE '^active 1 [0-9]+ 700 [1-9]'; then
            fail rpm "after rpm -i: $installed (expected: active, an agent, the user, its folder 700, /dev/uinput)"
        elif [ -n "$left" ]; then
            fail rpm "rpm -e left: $(echo $left)"
        else
            pass rpm
        fi
        ;;
    purge)
        vm ssh "sudo apt-get remove --purge -y queqiao > /tmp/purge.txt 2>&1; tail -1 /tmp/purge.txt" > "$out/purge.txt" 2>&1
        left=$(vm ssh "ls -d /usr/lib/queqiao /var/lib/queqiao /var/lib/queqiao-signin /usr/lib/systemd/system/queqiao.service 2> /dev/null; getent passwd queqiao; systemctl is-active queqiao 2> /dev/null | grep -x active" 2>&1)
        if [ -n "$left" ]; then
            fail purge "left behind: $(echo $left)"
        else
            pass purge
        fi
        ;;
    *) fail "$step" "no such step" ;;
    esac
done
# The server stays for the next run (its account in $out/server).
[ $failed = 0 ] && echo SERVICE_VM_PASS || echo SERVICE_VM_FAIL
