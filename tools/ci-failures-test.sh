#!/bin/sh
# Check the log parser of tools/ci-failures.sh on written samples of a Windows
# `cargo test --no-fail-fast` log and a Linux `mise run check` log that
# interleaves libtest and tools/luatest.sh output. The samples are made up.
#
# Usage: sh tools/ci-failures-test.sh
# POSIX sh.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

fail() {
    printf 'ci-failures-test: %s\n' "$1" >&2
    exit 1
}

parse() {
    sh "$ROOT/tools/ci-failures.sh" --parse <"$1"
}

ESC=$(printf '\033')
TS=2026-01-02T03:04:05.1234567Z

# Windows: timestamps, CRLF, failing tests in two binaries, a failures list.
printf '%s\r\n' \
    "$TS      Running unittests src/lib.rs (target\\debug\\deps\\alpha-1.exe)" \
    "$TS test ring::tests::wraps ... ok" \
    "$TS test ring::tests::splits ... FAILED" \
    "$TS " \
    "$TS failures:" \
    "$TS " \
    "$TS ---- ring::tests::splits stdout ----" \
    "$TS thread panicked at src/ring.rs:9:5:" \
    "$TS " \
    "$TS failures:" \
    "$TS     ring::tests::splits" \
    "$TS " \
    "$TS test result: FAILED. 1 passed; 1 failed; 0 ignored" \
    "$TS error: test failed, to rerun pass \`-p alpha --lib\`" \
    "$TS test codec::tests::empty ... FAILED" \
    "$TS test codec::tests::full ... ok" \
    "$TS error: 2 targets failed:" \
    >"$SCRATCH/windows.log"

out=$(parse "$SCRATCH/windows.log")
want="test ring::tests::splits
test codec::tests::empty"
[ "$out" = "$want" ] || fail "windows log: got: $out"

# Linux: libtest with colors, a Lua script that passes, one that fails.
{
    printf '%s %s[32mtest%s[0m a::b ... ok\n' "$TS" "$ESC" "$ESC"
    printf '%s test a::c ... %s[31mFAILED%s[0m\n' "$TS" "$ESC" "$ESC"
    printf '%s failures:\n' "$TS"
    printf '%s     a::c\n' "$TS"
    printf '%s \n' "$TS"
    printf '%s --- lua/tests/first.lua\n' "$TS"
    printf '%s ok  it loads\n' "$TS"
    printf '%s --- lua/tests/second.lua\n' "$TS"
    printf '%s ok  it starts\n' "$TS"
    printf '%s lua: lua/tests/second.lua:42: expected 3, got 4\n' "$TS"
    printf '%s stack traceback:\n' "$TS"
    printf '%s \t[C]: in function error\n' "$TS"
} >"$SCRATCH/linux.log"

out=$(parse "$SCRATCH/linux.log")
want="test a::c
lua lua/tests/second.lua: lua/tests/second.lua:42: expected 3, got 4"
[ "$out" = "$want" ] || fail "linux log: got: $out"

# A failed step with no test name prints nothing from the parser.
printf '%s error: buf lint failed\n' "$TS" >"$SCRATCH/step.log"
out=$(parse "$SCRATCH/step.log")
[ -z "$out" ] || fail "step log: got: $out"

# A passing log prints nothing.
printf '%s\n' 'test a::b ... ok' 'test result: ok. 1 passed' >"$SCRATCH/pass.log"
out=$(parse "$SCRATCH/pass.log")
[ -z "$out" ] || fail "passing log: got: $out"

# A usage error exits 2 without reaching the network.
if sh "$ROOT/tools/ci-failures.sh" >/dev/null 2>&1; then fail "no argument passed"; fi

printf 'ci-failures-test: ok\n'
