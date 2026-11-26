#!/bin/sh
# Run one typed-shapes snippet through the eval channel of <server> and print the JSON
# result.
#   run.sh <server> draw|radius|clear
set -eu
here=$(cd "$(dirname "$0")" && pwd)
[ $# -eq 2 ] || { echo 'usage: run.sh <server> draw|radius|clear' >&2; exit 2; }
case $2 in
  draw|radius|clear) ;;
  *) echo "unknown step: $2" >&2; exit 2 ;;
esac
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed -e "s/<step>/$2/" "$here/rotated.lua" > "$tmp/snippet.lua"
cargo run -q --manifest-path "$here/../../../Cargo.toml" -p xtask -- eval "$1" server "$tmp/snippet.lua"
