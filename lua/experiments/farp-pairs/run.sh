#!/bin/sh
# Run one FARP-pair snippet through the eval channel of <server>, printing the JSON
# result. Load the heliport-group mission first (see docs/results).
#   run.sh <server> make <group-static|static-units|same-group|group-ground|group-dynamic>
#   run.sh <server> parking <airbase name>
set -eu
[ $# -eq 3 ] || { echo 'usage: run.sh <server> make|parking <try or airbase name>' >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
case $2 in
  make)
    case $3 in
      group-static|static-units|same-group|group-ground|group-dynamic) ;;
      *) echo "unknown try: $3" >&2; exit 2 ;;
    esac
    sed -e "s/<try>/$3/" "$here/make.lua" > "$tmp/snippet.lua" ;;
  parking)
    case $3 in
      *[!A-Za-z0-9_-]*|'') echo "airbase name must be letters, digits, - or _: $3" >&2; exit 2 ;;
    esac
    sed -e "s/<name>/$3/" "$here/parking.lua" > "$tmp/snippet.lua" ;;
  *) echo "unknown snippet: $2" >&2; exit 2 ;;
esac
cargo run -q --manifest-path "$here/../../../Cargo.toml" -p xtask -- eval "$1" server "$tmp/snippet.lua"
