#!/bin/sh
# PS4 port: the PS4 app on the PC's fake GNM GPU (same source, same settings as the console's seed), profiled. The run loads the
# first save of card.bin by the scripted pad, walks in level 7 and stops after FIELDS fields; RAC_PS4_PROF samples every thread
# from START seconds for SECS seconds. Output in build-ps4/profrun: stderr.txt (the game's log, GNM stats lines), profile.txt,
# then the per-thread table (ps4/prof.sh).
#   sh ps4/pc-prof.sh [START=40] [SECS=20] [FIELDS=3000]     extra env: anything in the environment passes through (RAC_*).
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
START=${1:-40}
SECS=${2:-20}
FIELDS=${3:-3000}
RUN="${PC_RUN:-$ROOT/build-ps4/profrun}"
mkdir -p "$RUN/mods"
# The fake run's settings (the console's seed: lighting enhanced, vegetation lush, GI high, draw high; the three PS4 mods) and a
# fresh copy of its card.
cp "$ROOT/ps4/app/seed/pc_options.txt" "$RUN/"
cp "$ROOT/build-ps4/fakerun3/card.keep.bin" "$RUN/card.bin"
cp "$ROOT/ps4/app/seed/mods/active.txt" "$ROOT/ps4/app/seed/mods/settings.json" "$RUN/mods/"
# PC_RUN=DIR runs in another folder (two runs side by side); RAC_PS4_PCOPT=shadows=off,... sets graphics options for the run as
# the console's run files do (each key=value replaces that key's line).
for kv in $(echo "${RAC_PS4_PCOPT:-}" | tr ',' ' '); do
	grep -v "^${kv%%=*}=" "$RUN/pc_options.txt" > "$RUN/pc_options.new" || true
	echo "$kv" >> "$RUN/pc_options.new"
	mv "$RUN/pc_options.new" "$RUN/pc_options.txt"
done
rm -f "$RUN/profile.txt" "$RUN/missing_pipelines.txt"
ISO="$ROOT/build-ps4/shadps4/data/rac-rs/game.iso"
export ORACLE_ISO="$ISO"
export ORACLE_STUBS="$ROOT/oracle/stubs.txt"
export ORACLE_HAND_SKIP="$(head -1 "$ROOT/oracle/hand/hand_skip.txt" | tr -d '\r')"
export RAC_PLAYER_DIR="$RUN" RAC_MODS_DIR="$RUN/mods"
export ORACLE_EE_STACK_MB=32 RAC_THREADS=${RAC_THREADS:-4}
export WGPU_BACKEND=noop WGPU_NOOP_BACKEND=1
export RAC_PS4_SHADERS="${RAC_PS4_SHADERS:-$ROOT/build-ps4/ps4shaders.bin}" RAC_PS4_MISSING="$RUN/missing_pipelines.txt"
export RAC_RETAIL_SMOKE=$FIELDS RAC_MUTE=1
export RAC_RETAIL_PAD="${RAC_RETAIL_PAD:-220-226:800:80,80;280-286:4000:80,80;320-326:40:80,80;420-426:40:80,80;1300-2900:0:80,0}"
export RAC_PS4_PROF="$START,$SECS"
# Closer to the console: the fake GPU's CPU-side DMA copies are left out (the console's GPU does them), and the hero is built
# on the GPU (the console's self-check passes; the fake's cannot, so it would fall back to the CPU build).
export RAC_GNM_FAKE_DMA=${RAC_GNM_FAKE_DMA:-0} RAC_HERO_GPU=${RAC_HERO_GPU:-1}
EXE="$ROOT/build-ps4/target/release/rac-ps4-app.exe"
"$EXE" view "$ISO" 0 --retail-cold --modern-cam > "$RUN/stdout.txt" 2> "$RUN/stderr.txt" || echo "exit $?"
grep -E "^GNM: stats|^prof:|front end field|level.*field" "$RUN/stderr.txt" | tail -15
[ -f "$RUN/profile.txt" ] && sh "$ROOT/ps4/prof.sh" "$RUN/profile.txt" "$EXE"
