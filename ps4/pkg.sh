#!/bin/sh
# PS4 port: wraps build-ps4/<name>/eboot.bin (from ps4/build.sh) into a fake PKG that a jailbroken PS4 (GoldHEN) installs.
#   sh ps4/pkg.sh hello|probe      -> build-ps4/<name>/<CONTENT_ID>.pkg
# The probe's PKG holds the recompiled game code, made from your own disc: it is for your own console only, never share it.
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NAME=${1:-probe}
OO="$ROOT/toolchain/openorbis/OpenOrbis/PS4Toolchain"
OUT="$ROOT/build-ps4/$NAME"
W() { cygpath -m "$1" 2>/dev/null || echo "$1"; }
export OO_PS4_TOOLCHAIN="$(W "$OO")"
# PkgTool is built for .NET Core 3.0; let it run on whatever newer runtime is installed.
export DOTNET_ROLL_FORWARD=Major
PKGTOOL="$OO/bin/windows/PkgTool.Core.exe"

case "$NAME" in
	hello) TITLE="RAC-RS PS4 std test"; TITLE_ID=RACR00001; LABEL=RACRSHELLO000000 ;;
	probe) TITLE="RAC-RS PS4 core probe"; TITLE_ID=RACR00002; LABEL=RACRSPROBE000000 ;;
	gpu) TITLE="RAC-RS PS4 GPU test"; TITLE_ID=RACR00003; LABEL=RACRSGPUTEST0000 ;;
	gnm) TITLE="RAC-RS PS4 GNM triangle"; TITLE_ID=RACR00004; LABEL=RACRSGNMTEST0000 ;;
	gnmtest) TITLE="RAC-RS PS4 GNM test 2"; TITLE_ID=RACR00005; LABEL=RACRSGNMTEST0002 ;;
	wgputest) TITLE="RAC-RS PS4 wgpu test"; TITLE_ID=RACR00006; LABEL=RACRSWGPUTEST000 ;;
	# APP_VARIANT=2: the same app under a second title id, so a newer build installs next to an older one (both use /data/rac-rs).
	# APP_VARIANT=3: the diagnostics build after the first console run froze the console (log saved every second, speed lines,
	# GPU hang detection, RAC_GNM_SKIP, RAC_PS4_SHOT).
	app) case "${APP_VARIANT:-1}" in
		2) TITLE="RAC-RS PS4 (pad, sound)"; TITLE_ID=RACR00008; LABEL=RACRSAPP00000002 ;;
		3) TITLE="RAC-RS PS4 (diagnostics)"; TITLE_ID=RACR00009; LABEL=RACRSAPP00000003 ;;
		*) TITLE="RAC-RS PS4"; TITLE_ID=RACR00007; LABEL=RACRSAPP00000000 ;;
	esac ;;
	*) echo "unknown program $NAME"; exit 1 ;;
esac
VERSION=01.00
CONTENT_ID="IV0000-${TITLE_ID}_00-${LABEL}"

# The package tree: eboot, system files from the OpenOrbis sample (icon, the "about" module every homebrew carries, the libc
# and Fios2 modules the loader expects) and the files the program reads from /app0.
P="$OUT/pkg"
rm -rf "$P"
mkdir -p "$P/sce_sys/about" "$P/sce_module"
S="$OO/samples/hello_world"
cp "$OUT/eboot.bin" "$P/"
cp "$S/sce_sys/about/right.sprx" "$P/sce_sys/about/"
cp "$S/sce_sys/icon0.png" "$P/sce_sys/"
cp "$S/sce_module/"*.prx "$P/sce_module/"
if [ "$NAME" = probe ] || [ "$NAME" = app ]; then
	cp "$ROOT/oracle/stubs.txt" "$P/stubs.txt"
fi
# The game: its shader cache (sh ps4/tools/shcache.sh, from a PC capture of the game) and the list of hands to skip.
if [ "$NAME" = app ]; then
	[ -f "$ROOT/build-ps4/ps4shaders.bin" ] || { echo "no build-ps4/ps4shaders.bin (sh ps4/tools/shcache.sh)"; exit 1; }
	cp "$ROOT/build-ps4/ps4shaders.bin" "$P/ps4shaders.bin"
	cp "$ROOT/oracle/hand/hand_skip.txt" "$P/hand_skip.txt"
	# First-start settings (graphics options and the PS4 mods, as in the shader capture), copied into /data/rac-rs when missing.
	# Flat names: create-gp4 lists only its own fixed folders, and PkgTool fails on a file in any other folder.
	cp "$ROOT/ps4/app/seed/pc_options.txt" "$P/seed_pc_options.txt"
	cp "$ROOT/ps4/app/seed/mods/active.txt" "$P/seed_mods_active.txt"
	cp "$ROOT/ps4/app/seed/mods/settings.json" "$P/seed_mods_settings.json"
fi
# The wgpu test's shader cache (made on the PC: see ps4/wgputest/Cargo.toml).
if [ "$NAME" = wgputest ]; then
	[ -f "$ROOT/build-ps4/wgputest-shaders.bin" ] || { echo "no build-ps4/wgputest-shaders.bin (see ps4/wgputest/Cargo.toml)"; exit 1; }
	cp "$ROOT/build-ps4/wgputest-shaders.bin" "$P/ps4shaders.bin"
fi

cd "$P"
SFO=sce_sys/param.sfo
"$PKGTOOL" sfo_new $SFO
"$PKGTOOL" sfo_setentry $SFO APP_TYPE --type Integer --maxsize 4 --value 1
"$PKGTOOL" sfo_setentry $SFO APP_VER --type Utf8 --maxsize 8 --value "$VERSION"
"$PKGTOOL" sfo_setentry $SFO ATTRIBUTE --type Integer --maxsize 4 --value 0
"$PKGTOOL" sfo_setentry $SFO CATEGORY --type Utf8 --maxsize 4 --value gd
"$PKGTOOL" sfo_setentry $SFO CONTENT_ID --type Utf8 --maxsize 48 --value "$CONTENT_ID"
"$PKGTOOL" sfo_setentry $SFO DOWNLOAD_DATA_SIZE --type Integer --maxsize 4 --value 0
"$PKGTOOL" sfo_setentry $SFO SYSTEM_VER --type Integer --maxsize 4 --value 0
"$PKGTOOL" sfo_setentry $SFO TITLE --type Utf8 --maxsize 128 --value "$TITLE"
"$PKGTOOL" sfo_setentry $SFO TITLE_ID --type Utf8 --maxsize 12 --value "$TITLE_ID"
"$PKGTOOL" sfo_setentry $SFO VERSION --type Utf8 --maxsize 8 --value "$VERSION"

FILES=$(find . -type f ! -name pkg.gp4 | sed 's|^\./||' | tr '\n' ' ')
"$OO/bin/windows/create-gp4.exe" -out pkg.gp4 --content-id="$CONTENT_ID" --files "$FILES"
"$PKGTOOL" pkg_build pkg.gp4 "$(W "$OUT")"
ls -la "$OUT"/*.pkg
