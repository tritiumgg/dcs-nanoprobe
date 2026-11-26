#!/bin/sh
# Run one drawing-layers snippet through the eval channel of <server> and print
# the JSON result.
#   run.sh <server> order|points|ids|bench|reid|reload|restart
#   run.sh <server> client <place|readd|newid|over|inplace|geometry|flicker|highids|watch|read|clear>
#   run.sh <server> partc <list|draw|move|refresh|other|bars|sweep|shapes2|rbars|rsweep|rhalf|clear>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
xtask() { cargo run -q --manifest-path "$here/../../../Cargo.toml" -p xtask -- "$@"; }
case ${2:-} in
  order|points|ids|bench|reid) [ $# -eq 2 ] || exit 2; xtask eval "$1" server "$here/$2.lua" ;;
  reload) [ $# -eq 2 ] || exit 2; xtask eval "$1" hook "$here/reload.lua" ;;
  restart) [ $# -eq 2 ] || exit 2; xtask restart "$1" ;;
  client)
    [ $# -eq 3 ] || { echo 'usage: run.sh <server> client <step>' >&2; exit 2; }
    case $3 in
      place|readd|newid|over|inplace|geometry|flicker|highids|watch|read|clear) ;;
      *) echo "unknown step: $3" >&2; exit 2 ;;
    esac
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    sed -e "s/<step>/$3/" "$here/client.lua" > "$tmp/snippet.lua"
    xtask eval "$1" server "$tmp/snippet.lua" ;;
  partc)
    [ $# -eq 3 ] || { echo 'usage: run.sh <server> partc <step>' >&2; exit 2; }
    case $3 in
      list|draw|move|refresh|other|bars|sweep|shapes2|rbars|rsweep|rhalf|clear) ;;
      *) echo "unknown step: $3" >&2; exit 2 ;;
    esac
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    sed -e "s/<step>/$3/" "$here/partc.lua" > "$tmp/snippet.lua"
    xtask eval "$1" server "$tmp/snippet.lua" ;;
  *) echo 'usage: run.sh <server> order|points|ids|bench|reid|reload|restart|client <step>|partc <step>' >&2; exit 2 ;;
esac
