#!/bin/sh
#
# Proves a scenario can fail: applies a sed expression to a scratch copy of the spec, runs the
# scenario with the Swift runner, and reports whether it failed. Run on the Mac, from macos/.
#
#   sh scripts/mutate-check.sh domain/navigation/latest-request-wins 's/"status": "ready"/"status": "idle"/'
set -eu
id="$1"
expr="$2"
spec="$(cd .. && pwd)/spec"
scratch="$(mktemp -d)"
cp -R "$spec" "$scratch/spec"
file="$scratch/spec/scenarios/$id.json"
cp "$file" "$scratch/original.json"
sed -i '' "$expr" "$file"
if cmp -s "$file" "$scratch/original.json"; then echo "NO-OP MUTATION: $id"; rm -rf "$scratch"; exit 0; fi
cd Packages/ExplorerSpecRunner
if SPEC_ROOT="$scratch/spec" swift run -q spec-runner "$id" 2>&1 | grep -q "FAILED"; then
    echo "caught: $id"
else
    echo "NOT CAUGHT: $id"
fi
rm -rf "$scratch"
