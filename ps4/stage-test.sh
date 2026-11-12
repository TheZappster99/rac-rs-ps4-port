#!/bin/sh
# PS4 port: puts everything the next console test needs into one folder, build-ps4/to-console (emptied first):
# the package(s) to install, env.txt and STEPS.txt from ps4/console-tests/current. The folder is under build-ps4 because the
# packages hold disc-derived code and must never be shared or committed.
#   sh ps4/stage-test.sh build-ps4/app/IV0000-RACR00009_00-RACRSAPP00000003.pkg [more.pkg ...]
# STAGE=next stages the round after (ps4/console-tests/current-next -> build-ps4/to-console-next) without touching the
# current one, for a round prepared while the previous one still waits for the console.
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SUF=${STAGE:+-$STAGE}
OUT="$ROOT/build-ps4/to-console$SUF"
rm -rf "$OUT"
mkdir -p "$OUT"
cp -r "$ROOT/ps4/console-tests/current$SUF/"* "$OUT/"
for p in "$@"; do cp "$p" "$OUT/"; done
ls -la "$OUT"
