#!/bin/sh
# Run one unknown markupToAll case through the eval channel of <server>. MAY CRASH DCS.
#   probe.sh <server> <shape> oval|points   (shape 2 or 8 to 12: no known drawing; shape 8
#   crashed DCS; the owner skipped 9 to 12)
#   probe.sh <server> 0 clear
set -eu
here=$(cd "$(dirname "$0")" && pwd)
[ $# -eq 3 ] || { echo 'usage: probe.sh <server> <shape> oval|points|clear' >&2; exit 2; }
case $2 in 0|2|8|9|10|11|12) ;; *) echo 'the shape is 2 or 8 to 12 (0 with clear)' >&2; exit 2 ;; esac
case $3 in oval|points|clear) ;; *) echo "unknown layout: $3" >&2; exit 2 ;; esac
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed -e "s/<shape>/$2/" -e "s/<layout>/$3/" "$here/unknown.lua" > "$tmp/snippet.lua"
cargo run -q --manifest-path "$here/../../../Cargo.toml" -p xtask -- eval "$1" server "$tmp/snippet.lua"
