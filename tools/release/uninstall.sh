#!/bin/sh
# Remove nanoprobe from a DCS Saved Games profile: runs `nanoprobe uninstall`
# with the program beside this script, and passes on every argument and the
# exit code. README.txt has the arguments.
exec "$(dirname -- "$0")/nanoprobe" uninstall "$@"
