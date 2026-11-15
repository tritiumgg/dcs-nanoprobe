#!/bin/sh
# Stage the release archives for Windows and Linux.
#
# Both archives carry the same payload: the loader, the agent DLL and the
# version's Lua, which `nanoprobe install` puts in a DCS Saved Games profile.
# A Linux DCS server runs under Wine, so its archive carries the Windows DLL
# too. Only the `nanoprobe` program differs: a Windows executable, or a static
# Linux one.
#
#   dist/nanoprobe-<v>-windows-x86_64.zip
#   dist/nanoprobe-<v>-linux-x86_64.tar.gz
#   dist/nanoprobe-<v>-agent.pdb     symbols for crash records; not in an archive
#   dist/SHA256SUMS                  over the three files above
#
# Each archive holds one folder, nanoprobe-<v>-<platform>/, with:
#
#   nanoprobe[.exe]                  doctor, install, uninstall, inspect, top, watch
#   install.cmd, uninstall.cmd       Windows only: run the program on a double click
#   LICENSE
#   payload/nanoprobe-hook.lua
#   payload/nanoprobe_agent.dll
#   payload/lua/hook.lua, log.lua, shim.lua
#
# Run it through `mise run package`, which first builds the DLL and checks it
# with tools/checkdll.sh. This script builds the program for both platforms.
#
# Usage: sh tools/stage-release.sh [out-dir]
# POSIX sh. Needs cargo, cargo-xwin (on macOS and Linux), zip, tar, and
# sha256sum or shasum.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OUT=${1:-$ROOT/dist}
cd "$ROOT"

VERSION=$(awk '
    /^\[workspace\.package\]/ { in_sec = 1; next }
    /^\[/ { in_sec = 0 }
    in_sec && /^version[[:space:]]*=/ {
        sub(/^version[[:space:]]*=[[:space:]]*"/, "")
        sub(/".*$/, "")
        print; exit
    }
' Cargo.toml)
[ -n "$VERSION" ] || { printf 'no version under [workspace.package]\n' >&2; exit 2; }

WIN=x86_64-pc-windows-msvc
LINUX=x86_64-unknown-linux-musl
DLL=target/$WIN/release/nanoprobe.dll
PDB=target/$WIN/release/nanoprobe.pdb
[ -f "$DLL" ] || { printf 'no DLL at %s. Run: mise run dll\n' "$DLL" >&2; exit 2; }
[ -f "$PDB" ] || { printf 'no PDB at %s. Run: mise run dll\n' "$PDB" >&2; exit 2; }

# The DLL must be the shipping build. tools/checkdll.sh refuses a faults
# build; run it again here so the script cannot stage one on its own.
sh tools/checkdll.sh "$DLL" >/dev/null

# The program builds into its own target folder. In the shared one, the
# Windows build's nanoprobe.pdb would replace the agent's, which has the same
# name.
CLI_TARGET=target/release-cli
case $(uname -s) in
    MINGW*|MSYS*|CYGWIN*|Windows_NT)
        cargo build --release -p nanoprobe-cli --target "$WIN" --target-dir "$CLI_TARGET" ;;
    *) cargo xwin build --release -p nanoprobe-cli --target "$WIN" --target-dir "$CLI_TARGET" ;;
esac
cargo build --release -p nanoprobe-cli --target "$LINUX" --target-dir "$CLI_TARGET"

sha() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

# Clear only what an earlier run of this script wrote.
mkdir -p "$OUT"
rm -rf "$OUT/stage"
rm -f "$OUT"/nanoprobe-*-windows-x86_64.zip "$OUT"/nanoprobe-*-linux-x86_64.tar.gz \
    "$OUT"/nanoprobe-*-agent.pdb "$OUT/SHA256SUMS"

# stage <platform> <program>
stage() {
    dir=$OUT/stage/nanoprobe-$VERSION-$1
    mkdir -p "$dir/payload/lua"
    cp "$2" "$dir/"
    cp LICENSE "$dir/LICENSE"
    cp lua/hook/nanoprobe-hook.lua "$dir/payload/nanoprobe-hook.lua"
    cp "$DLL" "$dir/payload/nanoprobe_agent.dll"
    for name in hook.lua log.lua shim.lua; do
        cp "lua/agent/$name" "$dir/payload/lua/$name"
    done
    chmod 0644 "$dir/LICENSE" "$dir/payload/nanoprobe-hook.lua" \
        "$dir/payload/nanoprobe_agent.dll" "$dir/payload/lua/"*.lua
}

stage windows-x86_64 "$CLI_TARGET/$WIN/release/nanoprobe.exe"
WINDIR=$OUT/stage/nanoprobe-$VERSION-windows-x86_64
for verb in install uninstall; do
    printf '@echo off\r\n"%%~dp0nanoprobe.exe" %s %%*\r\npause\r\n' "$verb" >"$WINDIR/$verb.cmd"
done

stage linux-x86_64 "$CLI_TARGET/$LINUX/release/nanoprobe"
chmod 0755 "$OUT/stage/nanoprobe-$VERSION-linux-x86_64/nanoprobe"

# Archive entries in a fixed order, without macOS metadata files.
(
    cd "$OUT/stage"
    name=nanoprobe-$VERSION-windows-x86_64
    find "$name" -type f | LC_ALL=C sort | zip -q -X "../$name.zip" -@
    name=nanoprobe-$VERSION-linux-x86_64
    find "$name" | LC_ALL=C sort >../.list
    COPYFILE_DISABLE=1 tar --numeric-owner --no-recursion -cf - -T ../.list | gzip -n -9 >"../$name.tar.gz"
    rm ../.list
)
rm -rf "$OUT/stage"

cp "$PDB" "$OUT/nanoprobe-$VERSION-agent.pdb"

(
    cd "$OUT"
    sha nanoprobe-"$VERSION"-* >SHA256SUMS
)
printf 'staged in %s:\n' "$OUT"
cat "$OUT/SHA256SUMS"
