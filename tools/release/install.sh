#!/bin/sh
# Install nanoprobe into a DCS Saved Games profile: runs `nanoprobe install`
# with the program beside this script, and passes on every argument and the
# exit code. README.txt has the arguments.
exec "$(dirname -- "$0")/nanoprobe" install "$@"
