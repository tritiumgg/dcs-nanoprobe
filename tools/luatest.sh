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

MODULE=
for candidate in \
    "$ROOT/target/debug/libnanoprobe.so" \
    "$ROOT/target/debug/libnanoprobe.dylib"
do
    [ -f "$candidate" ] && MODULE=$candidate
done

[ -n "$MODULE" ] || {
    printf 'no libnanoprobe.so or libnanoprobe.dylib under %s/target/debug\n' "$ROOT" >&2
    exit 1
}

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

# Every script under lua/tests, in name order. The first failure stops the run.
for script in "$ROOT"/lua/tests/*.lua; do
    printf '%s\n' "--- ${script#"$ROOT"/}"
    "$LUA" "$script" "$MODULE" "$VERSION"
done
