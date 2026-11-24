#!/bin/bash
# The Linux counterpart of scripts\check.ps1 (docs/LINUX.md): formatting, clippy with warnings as errors (the shipping
# configuration of the programs with test options, then the test build of the whole workspace), the build, the unit
# tests, and every program's --version. Prints one line per step and CHECK_PASS or CHECK_FAIL <step> last; the full
# cargo output goes to target/check.log (or $CARGO_TARGET_DIR/check.log).
# Usage: scripts/check.sh   (from anywhere; QUEQIAO_CHECK_JOBS limits cargo's jobs)
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root" || exit 1
target="${CARGO_TARGET_DIR:-$root/target}"
mkdir -p "$target"
log="$target/check.log"
: > "$log"
jobs=()
if [ -n "${QUEQIAO_CHECK_JOBS:-}" ]; then jobs=(-j "$QUEQIAO_CHECK_JOBS"); fi
features='queqiao-agent/test-switches,queqiao-viewer/test-switches,queqiao-service/test-switches'
shipping=(-p queqiao-agent -p queqiao-viewer -p queqiao-service)
# Libraries, programs and tests; the examples are the Windows probes (encode_probe, capture_probe, ...) until their
# Linux counterparts exist (docs/LINUX.md step 3), so they are left out here.
targets=(--lib --bins --tests)
# The three programs with test options have no library.
program_targets=(--bins --tests)
failed=''

step() {
    local name="$1"; shift
    local started=$SECONDS
    echo "== cargo $*" >> "$log"
    cargo "$@" >> "$log" 2>&1
    local code=$?
    printf '%-8s exit %s  %s s\n' "$name" "$code" "$((SECONDS - started))"
    if [ "$code" -ne 0 ]; then failed="$name"; fi
    return "$code"
}

# Scripts stay ASCII (the PowerShell ones are read as ANSI on Windows; the shell ones are kept alike).
bad=$(grep -rlP '[^\x00-\x7F]' --include='*.sh' --include='*.ps1' scripts 2>/dev/null)
if [ -n "$bad" ]; then echo "scripts  non-ASCII: $bad"; failed='scripts'; else echo 'scripts  ascii ok'; fi

[ -z "$failed" ] && step fmt fmt --all --check
[ -z "$failed" ] && step clippy-s clippy "${jobs[@]}" "${shipping[@]}" "${program_targets[@]}" -- -D warnings
[ -z "$failed" ] && step test-s test "${jobs[@]}" "${shipping[@]}" "${program_targets[@]}"
[ -z "$failed" ] && step clippy clippy "${jobs[@]}" --features "$features" --workspace "${targets[@]}" -- -D warnings
[ -z "$failed" ] && step build build "${jobs[@]}" --features "$features" --workspace "${targets[@]}"
[ -z "$failed" ] && step test test "${jobs[@]}" --features "$features" --workspace "${targets[@]}"

# Every crate with src/main.rs is a program: --version must print its banner "<name> <version> ...".
if [ -z "$failed" ]; then
    for main in crates/*/src/main.rs; do
        name=$(basename "$(dirname "$(dirname "$main")")")
        out=$(timeout 20 "$target/debug/$name" --version 2>&1 | head -1)
        if [[ "$out" == "$name "* ]]; then echo "version  $out"; else echo "version  $name: '$out'"; failed='version'; fi
    done
fi

if [ -n "$failed" ]; then
    grep -E '^(error|warning)(\[|:)|^ *--> ' "$log" | head -40
    echo "CHECK_FAIL $failed (log: $log)"
    exit 1
fi
echo "CHECK_PASS (log: $log)"
