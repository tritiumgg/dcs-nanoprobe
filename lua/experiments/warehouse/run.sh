#!/bin/sh
# Run one warehouse step: fill step.lua's placeholders and run it through the
# eval channel of <server>, printing the JSON result.
#   run.sh <server> <action> [slot] [seconds] [stock]
# action: list | stock | slot | deslot | kill | none (see step.lua)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
server=$1
action=$2
slot=${3:-}
seconds=${4:-60}
stock=${5:-100}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed -e "s/<action>/$action/" -e "s/<slot>/$slot/" -e "s/<seconds>/$seconds/" \
  -e "s/<stock>/$stock/" "$here/step.lua" > "$tmp/step.lua"
timeout=$((seconds + 60))
cargo run -q -p xtask -- eval "$server" hook "$tmp/step.lua" --timeout "$timeout"
