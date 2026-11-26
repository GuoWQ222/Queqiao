#!/bin/bash
# The Linux keyring path (crates/queqiao-account/src/keyring.rs, config.rs protect_key) against a real GNOME Keyring:
# a D-Bus session of its own (dbus-run-session) with gnome-keyring-daemon (secrets only, its login keyring unlocked
# with a throwaway password), in a home and config folder of their own, then the ignored keyring test
# (the_key_file_moves_into_the_keyring). Nothing of the person's own keyring or files is touched.
# Needs: dbus-run-session, gnome-keyring-daemon. Prints KEYRING_PASS or KEYRING_FAIL <why>.
set -u
cd "$(dirname "$0")/.." || exit 1
for tool in dbus-run-session gnome-keyring-daemon; do
    command -v "$tool" > /dev/null || { echo "KEYRING_FAIL $tool is missing"; exit 1; }
done
export CARGO_HOME=${CARGO_HOME:-$HOME/.cargo} RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}
# Built first, with the person's home (cargo's own folders).
cargo test -q -p queqiao-account --no-run > /dev/null 2>&1 || { echo "KEYRING_FAIL build"; exit 1; }
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -m 700 "$scratch/run" "$scratch/config"
export HOME=$scratch XDG_RUNTIME_DIR=$scratch/run XDG_CONFIG_HOME=$scratch/config XDG_DATA_HOME=$scratch/data \
    QUEQIAO_TEST_KEYRING=1
output=$(dbus-run-session -- bash -c '
    printf throwaway | gnome-keyring-daemon --unlock --components=secrets > /dev/null 2>&1
    cargo test -q -p queqiao-account the_key_file_moves_into_the_keyring -- --ignored 2>&1
' 2>&1)
echo "$output" | tail -5
echo "$output" | grep -q "test result: ok. 1 passed" && echo KEYRING_PASS || echo "KEYRING_FAIL the test"
