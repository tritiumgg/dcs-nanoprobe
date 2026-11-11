#!/bin/sh
# Check the cross-built agent DLL: it imports DCS's Lua from lua.dll, links
# the C runtime statically, exports luaopen_nanoprobe, and carries the ABI
# marker.
#
# A DLL that links against the wrong Lua, or lacks the opener, builds without
# complaint and fails only inside DCS. This catches both on the build host.
#
# Usage: tools/checkdll.sh [path to nanoprobe.dll]
# POSIX sh. Needs llvm-readobj, which mise.toml's LLVM provides.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
DLL=${1:-$ROOT/target/x86_64-pc-windows-msvc/release/nanoprobe.dll}

[ -f "$DLL" ] || {
    printf 'no DLL at %s. Run: mise run windows\n' "$DLL" >&2
    exit 2
}

IMPORTS=$(llvm-readobj --coff-imports "$DLL")
EXPORTS=$(llvm-readobj --coff-exports "$DLL")
FAILED=0

fail() {
    printf 'FAIL  %s\n' "$1" >&2
    FAILED=1
}

case "$IMPORTS" in
    *"Name: lua.dll"*) printf 'ok  imports from lua.dll\n' ;;
    *) fail "the DLL does not import from lua.dll" ;;
esac

case "$IMPORTS" in
    *"Name: lua51.dll"* | *"Name: lua5.1.dll"*) fail "the DLL imports a Lua that DCS does not ship" ;;
esac

case "$IMPORTS" in
    *"Name: VCRUNTIME"* | *"Name: api-ms-win-crt-"* | *"Name: ucrtbase"*)
        fail "the DLL imports the dynamic C runtime; it must link it statically" ;;
    *) printf 'ok  links the C runtime statically\n' ;;
esac

case "$EXPORTS" in
    *"Name: luaopen_nanoprobe"*) printf 'ok  exports luaopen_nanoprobe\n' ;;
    *) fail "the DLL does not export luaopen_nanoprobe" ;;
esac

if LC_ALL=C grep -a -q 'nanoprobe-abi=' "$DLL"; then
    printf 'ok  carries the ABI marker %s\n' "$(LC_ALL=C grep -a -o 'nanoprobe-abi=[0-9]*' "$DLL" | head -n 1)"
else
    fail "the DLL carries no ABI marker"
fi

exit "$FAILED"
