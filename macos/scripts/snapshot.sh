#!/bin/sh
#
# Launches the built app on the Mac against a throwaway state folder, renders its window to a PNG
# and copies it here. Needs no screen-recording permission (the app draws its own window).
#
#   [SEED_VIEW='{"defaults":{"view_mode":"detailed-list"}}'] macos/scripts/snapshot.sh <folder-to-open> <out.png> [extra app arguments...]
set -eu
folder="$1"; out="$2"; shift 2
here="$(cd "$(dirname "$0")" && pwd)"
host="${MAC_HOST:-user@mac.example.com}"
"$here/mac-sync.sh" "pkill -f 'Morin Explorer' || true; rm -rf /tmp/ms-snap /tmp/snap-*.png; mkdir -p /tmp/ms-snap; if [ -n '${SEED_VIEW:-}' ]; then echo '${SEED_VIEW:-}' > /tmp/ms-snap/view-options.json; fi; \
    open -n '.dd/Build/Products/Debug/Morin Explorer.app' --args -MorinStateDirectory /tmp/ms-snap -MorinOpen '$folder' -MorinSnapshot /tmp/snap $*; \
    i=0; while pgrep -f '[M]orin Explorer.app/Contents/MacOS' >/dev/null && [ \$i -lt 40 ]; do sleep 1; i=\$((i+1)); done; ls /tmp/snap-*.png"
base="${out%.png}"
for n in 0 1 2; do scp -q "$host:/tmp/snap-$n.png" "$base-$n.png" 2>/dev/null || true; done
