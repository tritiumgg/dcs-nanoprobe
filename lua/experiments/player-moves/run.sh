#!/bin/sh
# Run a player-moves snippet through the eval channel, printing the JSON
# result.
#   run.sh <server> <steps> [settle]   moves.lua on the server
#   run.sh --client <client> <side>    client-side.lua on the client
# steps: see moves.lua, for example '1,;2,;2,;2,@blue;2,;1,@blue;0,'
set -eu
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
if [ "$1" = --client ]; then
    sed -e "s/<side>/$3/" "$here/client-side.lua" > "$tmp/client-side.lua"
    cargo run -q -p xtask -- eval "$2" hook "$tmp/client-side.lua" --timeout 60
    exit
fi
server=$1
steps=$2
settle=${3:-6}
sed -e "s/<steps>/$steps/" -e "s/<settle>/$settle/" "$here/moves.lua" > "$tmp/moves.lua"
cargo run -q -p xtask -- eval "$server" hook "$tmp/moves.lua" --timeout 960
