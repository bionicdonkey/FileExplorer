#!/bin/sh
#
# Builds and tests on the Mac mini from another machine: copies macos/, spec/ and the
# separation script to the mini, then runs a command there from the copied macos/ folder.
#
#   macos/scripts/mac-sync.sh                      swift test in every package
#   macos/scripts/mac-sync.sh 'sh scripts/xcode-test.sh'
#
# MAC_HOST (default user@mac.example.com) and MAC_DIR (default
# ~/work/FileExplorer) override where it goes.
set -eu
host="${MAC_HOST:-user@mac.example.com}"
dir="${MAC_DIR:-work/FileExplorer}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

ssh -o BatchMode=yes "$host" "mkdir -p $dir"
rsync -a --delete --exclude '.build' --exclude '.dd' --exclude '.swiftpm' --exclude 'xcuserdata' macos/ "$host:$dir/macos/"
rsync -a --delete spec/ "$host:$dir/spec/"
rsync -a scripts/ "$host:$dir/scripts/"

if [ "$#" -eq 0 ]; then
    set -- 'for p in Packages/*; do (cd "$p" && swift test 2>&1 | tail -25) || exit 1; done'
fi
ssh -o BatchMode=yes "$host" "cd $dir/macos && $*"
