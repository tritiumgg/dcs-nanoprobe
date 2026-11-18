#!/bin/sh
# Print the cargo `--config` value that keeps this machine's paths out of a
# release build for one target. A release is public, and rustc writes the
# absolute path of every source file into panic messages and debug
# information: the checkout, cargo's registry and the toolchain, each under
# the builder's home folder. The value maps the three to `nanoprobe`, `cargo`
# and `rust`.
#
# cargo joins a `--config` list to the list `.cargo/config.toml` holds for the
# same key, so the target's own flags stay. For Windows the value also makes
# the linker name the PDB in the program by its file name alone.
#
#   cargo build --release ... --target <triple> --config "$(sh tools/remap.sh <triple>)"
#
# tools/stage-release-test.sh checks the staged files for the paths. The paths
# are this shell's, so the maps match on macOS and Linux. On a Windows host
# rustc sees Windows paths and the maps match nothing.
#
# Usage: sh tools/remap.sh <target triple>
# POSIX sh. Needs rustc. A path with a single quote in it is refused.
set -eu

[ $# -eq 1 ] || { printf 'usage: sh tools/remap.sh <target triple>\n' >&2; exit 2; }
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
CARGO_DIR=${CARGO_HOME:-$HOME/.cargo}
SYSROOT=$(cd "$ROOT" && rustc --print sysroot)

flags=
add() {
    case $1 in
        *"'"*) printf 'tools/remap.sh: a single quote in %s\n' "$1" >&2; exit 2 ;;
    esac
    flags="$flags${flags:+, }'$1'"
}
add "--remap-path-prefix=$ROOT=nanoprobe"
add "--remap-path-prefix=$CARGO_DIR=cargo"
add "--remap-path-prefix=$SYSROOT=rust"
case $1 in
    *-windows-msvc) add "-Clink-arg=/PDBALTPATH:%_PDB%" ;;
esac
printf 'target.%s.rustflags=[%s]\n' "$1" "$flags"
