#!/bin/sh
# Build the Kotlin hello campaign, then run it against the fake sidecar on a
# free loopback port, with a time limit. Exit nonzero if the build or the hello
# fails or times out. The sidecar picks the port (port 0), runs the hello as its
# child with NANOPROBE_ADDR set, stops, and exits with the child's code.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
limit=${HELLO_CHECK_SECONDS:-60}

cd "$root"
# No Gradle daemon: nothing of the build outlives it.
gradle --no-daemon --quiet --project-dir "$here" installDist
cargo build -q -p nanoprobe-fake-sidecar --no-default-features
sidecar=${CARGO_TARGET_DIR:-$root/target}/debug/nanoprobe-fake-sidecar

"$sidecar" --listen 127.0.0.1:0 --run "$here/build/install/hello-kotlin/bin/hello-kotlin" &
pid=$!

# Watchdog: kill the sidecar if it outlives the limit. Its child dies with the
# closed stdout pipe, or with the sidecar's own cleanup.
( sleep "$limit"; pkill -P "$pid" 2>/dev/null; kill "$pid" 2>/dev/null ) &
watchdog=$!

status=0
wait "$pid" || status=$?
pkill -P "$watchdog" 2>/dev/null || true
kill "$watchdog" 2>/dev/null || true
wait "$watchdog" 2>/dev/null || true
if [ "$status" -ne 0 ]; then
  echo "hello-kotlin check: failed with status $status (limit ${limit}s)" >&2
  exit 1
fi
echo "hello-kotlin check: passed"
