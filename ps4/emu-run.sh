#!/bin/sh
# PS4 port: runs the app in the shadPS4 emulator on the PC, as a first check before a build goes to the console. shadPS4 is not
# the real GPU (pictures and hangs can differ), but crashes, missing functions and the CPU side show up here without the console.
#   sh ps4/emu-run.sh [env.txt] [seconds]      (defaults: ps4/console-tests/current/env.txt, 120)
# Needs build-ps4/shadps4/emu/shadPS4.exe (v0.21.0, github.com/shadps4-emu/shadPS4) and build-ps4/shadps4/data/rac-rs/game.iso
# (your own disc image). The emulator's /data is build-ps4/shadps4/data; this run's log, emulator log and pictures are copied
# to build-ps4/shadps4/last/. Everything stays under build-ps4 because it holds disc-derived code.
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
E="$ROOT/build-ps4/shadps4"
ENV=${1:-$ROOT/ps4/console-tests/current/env.txt}
SECS=${2:-120}
D="$E/data/rac-rs"
[ -f "$D/game.iso" ] || { echo "no $D/game.iso (copy your disc image there)"; exit 1; }
[ -f "$ROOT/build-ps4/app/pkg/eboot.bin" ] || { echo "no build-ps4/app/pkg (sh ps4/build.sh app && sh ps4/pkg.sh app)"; exit 1; }
# A clean start each time: no logs, pictures or run series from the last run; settings come from the package again.
# EMU_KEEP=1 keeps the settings, mods and a run series from the last start (several starts in a row, as the console does);
# ENV=- then leaves env.txt as it is.
if [ "${EMU_KEEP:-0}" = 1 ]; then
	rm -f "$D"/log*.txt "$D"/shot_*.bmp "$D"/missing_pipelines.txt
else
	rm -rf "$D"/log*.txt "$D"/shot_*.bmp "$D"/shots_run* "$D"/runs "$D"/missing_pipelines.txt "$D"/mods "$D"/pc_options.txt
fi
[ "$ENV" = - ] || cp "$ENV" "$D/env.txt"
W() { cygpath -w "$1" 2>/dev/null || echo "$1"; }
# MSYS_NO_PATHCONV: Git Bash would otherwise turn the guest path /data into a Windows path.
set +e
# The patched build (ps4/shadps4.patch: instructions Mesa's shader compiler emits that stock shadPS4 lacks; built by
# ps4/build-shadps4.cmd) when present, else the stock release.
EXE="$E/src/build/shadps4.exe"
[ -f "$EXE" ] || EXE="$E/emu/shadPS4.exe"
# By default shadPS4 drops all GPU work (its "null_gpu" setting), so only the CPU side runs: start-up, loading, game logic,
# threads, memory, the pad and the command buffers we build. EMU_GPU=1 tries the real GPU path, which today stops at the first
# compute shader: Mesa's compiler picks buffer descriptors at run time, which shadPS4's resource tracking cannot follow.
CFG="$APPDATA/shadPS4/config.json"
[ -f "$CFG" ] && { [ "${EMU_GPU:-0}" = 1 ] && N=false || N=true; sed -i "s/\"null_gpu\": [a-z]*/\"null_gpu\": $N/" "$CFG"; }
(cd "$E/emu" && MSYS_NO_PATHCONV=1 timeout "$SECS" "$EXE" --mount "$(W "$E/data")" /data -g "$(W "$ROOT/build-ps4/app/pkg/eboot.bin")" > "$E/emu_log.txt" 2>&1)
echo "shadPS4 ended ($?; 124 = stopped after $SECS s)"
set -e
# Emptied rather than removed: a shell sitting in the folder would make removing it fail.
mkdir -p "$E/last"
rm -rf "$E/last/"*
cp "$E/emu_log.txt" "$E/last/"
for f in "$D"/log*.txt "$D"/missing_pipelines.txt "$D"/shot_*.bmp; do [ -f "$f" ] && cp "$f" "$E/last/"; done
ls "$E/last"
