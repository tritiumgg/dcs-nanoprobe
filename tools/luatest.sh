#!/bin/sh
# Build the host-native agent and open it with a stock Lua 5.1.
#
# --no-default-features turns the dcs-lua feature off, which is what the
# host-native build is. Left on, the module would link DCS's lua.dll, and a
# machine without DCS has none to load.
#
# Linux and macOS only. A Windows module resolves no symbol at load, so a
# host-native build there would need a separate lua51.dll and a second .def for
# a configuration that never ships. The Windows cross-build proves the product
# DLL imports lua.dll.
#
# POSIX sh. Needs Lua 5.1, which mise.toml pins.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN* | Windows_NT)
        printf 'skipped: a Windows module resolves no Lua symbol at load.\n'
        exit 0
        ;;
esac

LUA=${LUA:-lua}
command -v "$LUA" >/dev/null 2>&1 || {
    printf 'no %s on PATH. Run: mise exec -- sh tools/luatest.sh\n' "$LUA" >&2
    exit 2
}

case "$("$LUA" -v 2>&1)" in
    *"Lua 5.1"*) ;;
    *)
        printf '%s is not Lua 5.1: %s\n' "$LUA" "$("$LUA" -v 2>&1)" >&2
        printf 'DCS embeds Lua 5.1, and the agent binds the stock 5.1 API.\n' >&2
        exit 2
        ;;
esac

(cd "$ROOT" && cargo build -p nanoprobe-agent --no-default-features)
# lua/tests/faults.lua needs fault injection, which the shipping build lacks.
# It gets its own build, in its own folder, so every other script tests the
# shipping feature set.
(cd "$ROOT" && cargo build -p nanoprobe-agent --no-default-features --features faults \
    --target-dir "$ROOT/target/faults")
# The sidecar side of the link rehearsal, lua/tests/link.lua.
(cd "$ROOT" && cargo build -p nanoprobe-fake-agent --bin link-probe --bin command-probe --bin command-script --bin conformance-probe -p nanoprobe-cli --bin nanoprobe -p nanoprobe-api --bin nanoprobe-sidecar -p nanoprobe-hello --bin hello-campaign)
PROBE="$ROOT/target/debug/link-probe"
INSPECT="$ROOT/target/debug/nanoprobe"
# The catalogue builder, which lua/tests/probes.lua runs on the scenarios'
# reports from the mock.
(cd "$ROOT" && cargo build -p xtask)
XTASK="$ROOT/target/debug/xtask"

# The host-native module under the folder $1.
module() {
    found=
    for candidate in "$1/libnanoprobe.so" "$1/libnanoprobe.dylib"; do
        [ -f "$candidate" ] && found=$candidate
    done
    [ -n "$found" ] || {
        printf 'no libnanoprobe.so or libnanoprobe.dylib under %s\n' "$1" >&2
        exit 1
    }
    printf '%s\n' "$found"
}
MODULE=$(module "$ROOT/target/debug")
FAULTS_MODULE=$(module "$ROOT/target/faults/debug")

# The version the module reports must match the workspace's, or the test proves
# only that some module opened.
VERSION=$(awk '
    /^\[workspace\.package\]/ { in_section = 1; next }
    /^\[/                     { in_section = 0 }
    in_section && /^version/  { gsub(/[^0-9A-Za-z.+-]/, "", $3); print $3; exit }
' "$ROOT/Cargo.toml")

[ -n "$VERSION" ] || {
    printf 'no [workspace.package] version in %s/Cargo.toml\n' "$ROOT" >&2
    exit 1
}

# The tests' scratch folders (lua/tests/support.lua, S.scratch) go under one
# folder that this run removes when it exits, however it exits.
NANOPROBE_SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/nanoprobe-luatest.XXXXXX")
export NANOPROBE_SCRATCH
trap 'rm -rf "$NANOPROBE_SCRATCH"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Every script under lua/tests, in name order, except the support files. The
# first failure stops the run.
for script in "$ROOT"/lua/tests/*.lua; do
    case ${script##*/} in support*) continue ;; esac
    printf '%s\n' "--- ${script#"$ROOT"/}"
    case ${script##*/} in
        faults.lua) "$LUA" "$script" "$FAULTS_MODULE" "$VERSION" "$PROBE" "$INSPECT" ;;
        # The mirror tests on the shipping build, then the tenth of the
        # sampling budget on the build with fault injection (spec/faults.md).
        sampling.lua)
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" match
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" deaths
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" sweep
            "$LUA" "$script" "$FAULTS_MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" tenth
            ;;
        # The agent's state lasts as long as the process: one scenario each.
        dispatch.lua)
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" no-shim
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" no-answer
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" budget
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" raw-off
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" unknown-extension
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" unknown-raw-off
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" unknown-no-shim
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" begin-only-pump
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" bounded-results
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" bad-code
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" deep-tables
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" world
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" world-gone
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" destroyed-overflow
            for name in hook-order hook-budget hook-no-result hook-no-shim sound clear-debris picture \
                marks menus message extension-eval chat mission-load players moderate \
                emit emit-objects emit-off extension-hooks extension-errors extension-budget \
                extension-records extension-env extension-stop menu-record; do
                "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" "$name"
            done
            ;;
        world_exit.lua)
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" groups
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" paused
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" names
            "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" debris
            ;;
        players_exit.lua)
            for name in functional-on functional-off mismatch moderation moderation-fails; do
                "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" "$name"
            done
            ;;
        *) "$LUA" "$script" "$MODULE" "$VERSION" "$PROBE" "$INSPECT" "$XTASK" ;;
    esac
done
