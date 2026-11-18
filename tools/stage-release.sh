#!/bin/sh
# Stage the release archives for Windows and Linux.
#
# Both archives carry the same payload: the loader, the agent DLL and the
# version's Lua, which `nanoprobe install` puts in a DCS Saved Games profile.
# A Linux DCS server runs under Wine, so its archive carries the Windows DLL
# too. The two programs differ: Windows executables, or static Linux ones.
#
#   dist/nanoprobe-<v>-windows-x86_64.zip
#   dist/nanoprobe-<v>-linux-x86_64.tar.gz
#   dist/nanoprobe-<v>-schema.tar.gz the API schema, and its descriptor set
#   dist/nanoprobe-<v>-schema.binpb  (tools/stage-schema.sh)
#   dist/SHA256SUMS                  over the four files above
#
# The release is those five files. The agent's PDB is not one of them: it
# names the build machine's folders, and the builder keeps it with the DLL to
# turn a crash record into function names (spec/install.md, "Release"). The
# folder is named by the signature a crash record's `pdb` carries
# (tools/pdbsig.sh), in the layout `cargo xtask symbolize` reads under
# target/symbols/. A later run adds its own folder and leaves the others:
#
#   dist/private/<signature>/nanoprobe.dll, nanoprobe.pdb
#
# Each archive holds one folder, nanoprobe-<v>-<platform>/, with:
#
#   nanoprobe[.exe]                  doctor, install, uninstall, inspect, top, watch,
#                                    restore
#   nanoprobe-sidecar[.exe]          the sidecar
#   install.cmd, uninstall.cmd       Windows: run the program on a double click
#   install.sh, uninstall.sh         Linux: run the program with their arguments
#   README.txt                       what the folder holds and the first steps
#   sidecar.toml                     Windows: an example for a sidecar beside DCS
#   deploy/                          Linux: the sidecar's container image, compose
#                                    overlay and example sidecar.toml (deploy/)
#   LICENSE
#   payload/nanoprobe-hook.lua
#   payload/nanoprobe_agent.dll
#   payload/lua/hook.lua, log.lua, shim.lua
#
# Run it through `mise run package`, which first builds the DLL and checks it
# with tools/checkdll.sh. This script builds the two programs for both
# platforms.
#
# Runs on macOS and Linux. A Windows host can build the DLL (`mise run dll`)
# but not stage a release: the Linux programs need a C compiler for musl, and
# tools/remap.sh maps POSIX paths.
#
# Usage: sh tools/stage-release.sh [out-dir]
# POSIX sh. Needs cargo, cargo-xwin, zig, buf, git, zip, tar, awk, and
# sha256sum or shasum.
set -eu

case $(uname -s) in
    MINGW* | MSYS* | CYGWIN* | Windows_NT)
        printf 'tools/stage-release.sh stages a release on macOS or Linux, not on Windows\n' >&2
        exit 2 ;;
esac

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
mkdir -p "${1:-$ROOT/dist}"
OUT=$(CDPATH= cd -- "${1:-$ROOT/dist}" && pwd)
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

# No program carries this machine's paths (tools/remap.sh). The programs build
# into their own target folder. In the shared one, the
# Windows build's nanoprobe.pdb would replace the agent's, which has the same
# name.
CLI_TARGET=target/release-cli
PROGRAMS='-p nanoprobe-cli --bin nanoprobe -p nanoprobe-api --bin nanoprobe-sidecar'
# shellcheck disable=SC2086 # PROGRAMS is a list of arguments.
cargo xwin build --release $PROGRAMS --target "$WIN" --target-dir "$CLI_TARGET" \
    --config "$(sh tools/remap.sh "$WIN")"
# Both programs hold the bundled SQLite, which needs a C compiler for musl:
# zig's, unless the caller names another.
: "${CC_x86_64_unknown_linux_musl:=$ROOT/tools/zigcc-musl.sh}"
: "${AR_x86_64_unknown_linux_musl:=$ROOT/tools/zigar.sh}"
export CC_x86_64_unknown_linux_musl AR_x86_64_unknown_linux_musl
# The C compiler writes its source paths too. The last map that matches a
# path applies, so the maps go from the widest to the narrowest. The cc crate
# splits these flags at white space and has no quoting.
CARGO_DIR=${CARGO_HOME:-$HOME/.cargo}
case "$HOME$ROOT$CARGO_DIR" in
    *[[:space:]]*)
        printf 'tools/stage-release.sh: white space in the path of the home folder, the checkout or cargo: the C compiler flags cannot carry it\n' >&2
        exit 2 ;;
esac
CFLAGS_x86_64_unknown_linux_musl="${CFLAGS_x86_64_unknown_linux_musl:-} -ffile-prefix-map=$HOME=home -ffile-prefix-map=$ROOT=nanoprobe -ffile-prefix-map=$CARGO_DIR=cargo"
export CFLAGS_x86_64_unknown_linux_musl
# shellcheck disable=SC2086
cargo build --release $PROGRAMS --target "$LINUX" --target-dir "$CLI_TARGET" \
    --config "$(sh tools/remap.sh "$LINUX")"

sha() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

# Clear only what an earlier run of this script wrote.
mkdir -p "$OUT"
rm -rf "$OUT/stage"
rm -f "$OUT"/nanoprobe-*-windows-x86_64.zip "$OUT"/nanoprobe-*-linux-x86_64.tar.gz \
    "$OUT"/nanoprobe-*-agent.pdb \
    "$OUT"/nanoprobe-*-schema.tar.gz "$OUT"/nanoprobe-*-schema.binpb "$OUT/SHA256SUMS"

# stage <platform> <program> <sidecar>
stage() {
    dir=$OUT/stage/nanoprobe-$VERSION-$1
    mkdir -p "$dir/payload/lua"
    cp "$2" "$3" "$dir/"
    cp LICENSE "$dir/LICENSE"
    cp lua/hook/nanoprobe-hook.lua "$dir/payload/nanoprobe-hook.lua"
    cp "$DLL" "$dir/payload/nanoprobe_agent.dll"
    for name in hook.lua log.lua shim.lua; do
        cp "lua/agent/$name" "$dir/payload/lua/$name"
    done
    chmod 0644 "$dir/LICENSE" "$dir/payload/nanoprobe-hook.lua" \
        "$dir/payload/nanoprobe_agent.dll" "$dir/payload/lua/"*.lua
}

# The text files a Windows user opens end their lines as Windows does.
crlf() {
    awk '{ sub(/\r$/, ""); printf "%s\r\n", $0 }' "$1" >"$2"
    chmod 0644 "$2"
}

stage windows-x86_64 "$CLI_TARGET/$WIN/release/nanoprobe.exe" \
    "$CLI_TARGET/$WIN/release/nanoprobe-sidecar.exe"
WINDIR=$OUT/stage/nanoprobe-$VERSION-windows-x86_64
for verb in install uninstall; do
    printf '@echo off\r\n"%%~dp0nanoprobe.exe" %s %%*\r\npause\r\n' "$verb" >"$WINDIR/$verb.cmd"
done
crlf tools/release/README.windows.txt "$WINDIR/README.txt"
crlf tools/release/sidecar.windows.toml "$WINDIR/sidecar.toml"

stage linux-x86_64 "$CLI_TARGET/$LINUX/release/nanoprobe" \
    "$CLI_TARGET/$LINUX/release/nanoprobe-sidecar"
LINUXDIR=$OUT/stage/nanoprobe-$VERSION-linux-x86_64
cp tools/release/install.sh tools/release/uninstall.sh "$LINUXDIR/"
chmod 0755 "$LINUXDIR/nanoprobe" "$LINUXDIR/nanoprobe-sidecar" \
    "$LINUXDIR/install.sh" "$LINUXDIR/uninstall.sh"
cp tools/release/README.linux.txt "$LINUXDIR/README.txt"
mkdir "$LINUXDIR/deploy"
# The files git tracks: a program copied into deploy/ for a local image build
# is not part of a release.
git ls-files -- deploy | while IFS= read -r f; do
    cp "$f" "$LINUXDIR/$f"
    chmod 0644 "$LINUXDIR/$f"
done
chmod 0644 "$LINUXDIR/README.txt"

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

SIG=$(sh tools/pdbsig.sh "$DLL")
mkdir -p "$OUT/private/$SIG"
cp "$DLL" "$OUT/private/$SIG/nanoprobe.dll"
cp "$PDB" "$OUT/private/$SIG/nanoprobe.pdb"
sh tools/stage-schema.sh "$OUT" "$VERSION" >/dev/null

(
    cd "$OUT"
    sha nanoprobe-"$VERSION"-* >SHA256SUMS
)
printf 'staged in %s:\n' "$OUT"
cat "$OUT/SHA256SUMS"
printf 'kept, not released: %s\n' "$OUT/private/$SIG/"
