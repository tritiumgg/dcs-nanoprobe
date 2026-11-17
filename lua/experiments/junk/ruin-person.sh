#!/bin/sh
# Run one step of ruin-person.lua:
#   ruin-person.sh <server> <spawn|kill|clear|destroy|read> [distance m] [power] [radius m]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed -e "s/<action>/$2/" -e "s/<distance>/${3:-300}/" -e "s/<power>/${4:-200}/" -e "s/<radius>/${5:-30}/" "$here/ruin-person.lua" > "$tmp/ruin-person.lua"
cargo run -q -p xtask -- eval "$1" server "$tmp/ruin-person.lua" --timeout 60
