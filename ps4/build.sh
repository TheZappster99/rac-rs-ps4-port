#!/bin/sh
# PS4 port: builds one of the ps4/ programs (probe = the game core, hello = a std smoke test) for the PS4 and turns it into a
# fake-signed eboot.bin. Needs the private toolchain with rust-src, the OpenOrbis toolchain in toolchain/openorbis and, for the
# probe, a completed setup (generated code in oracle/).
#   sh ps4/build.sh [probe|hello|gpu|gnm|gnmtest|wgputest|app] [cargo args]      -> build-ps4/<name>/eboot.bin (and the ELF next to it)
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NAME=${1:-probe}
[ $# -gt 0 ] && shift
T="$ROOT/toolchain"
OO="$T/openorbis/OpenOrbis/PS4Toolchain"
OUT="$ROOT/build-ps4/$NAME"
TD="$ROOT/build-ps4/target"
export RUSTUP_HOME="$T/rustup" CARGO_HOME="$T/cargo" RUSTUP_TOOLCHAIN=1.99.0-x86_64-pc-windows-gnu
# build-std on a stable toolchain.
export RUSTC_BOOTSTRAP=1
export PATH="$T/cargo/bin:$T/rustup/toolchains/1.99.0-x86_64-pc-windows-gnu/lib/rustlib/x86_64-pc-windows-gnu/bin:$PATH"

# std is built from a patched copy of the toolchain's rust-src (ps4/std.patch: libc in FreeBSD 11 mode without FreeBSD's symbol
# versions, the one std line that mode breaks, stdout / stderr into the app's log file). Remade whenever the patch is newer than
# the copy. Cargo's build-std does not notice edited std sources (it fingerprints std by version), so a remake also deletes
# std's fingerprints, which rebuilds std and everything above it.
SRC="$ROOT/build-ps4/std-src"
if [ ! -f "$SRC/.patched" ] || [ "$ROOT/ps4/std.patch" -nt "$SRC/.patched" ]; then
	rm -rf "$TD"/x86_64-rac-ps4/*/.fingerprint/std-*
	[ -d "$T/rustup/toolchains/1.99.0-x86_64-pc-windows-gnu/lib/rustlib/src/rust/library" ] || rustup component add rust-src
	rm -rf "$SRC"
	mkdir -p "$SRC"
	cp -r "$T/rustup/toolchains/1.99.0-x86_64-pc-windows-gnu/lib/rustlib/src/rust/library" "$SRC/"
	# A repository of its own: inside the game's repository `git apply` would skip every file without a word. The base commit
	# is the untouched rust-src, so `git diff` there regenerates ps4/std.patch after editing std-src by hand.
	(cd "$SRC" && git init -q && git add -A && git -c user.name=build -c user.email=build@localhost commit -qm "rust-src 1.99.0" 		&& git apply --whitespace=nowarn "$ROOT/ps4/std.patch" && git diff --quiet && { echo "std.patch applied nothing"; exit 1; } || true)
	grep -q rac_std_set_log_fd "$SRC/library/std/src/sys/stdio/unix.rs" || { echo "ps4/std.patch did not apply"; exit 1; }
	touch "$SRC/.patched"
fi
export __CARGO_TESTS_ONLY_SRC_ROOT="$SRC/library"

# The crates.io libc that other crates use (the game's winit -> rustix calls kevent) names the same FreeBSD symbol versions
# (kevent@FBSD_1.0, stat@FBSD_1.0, ...) that the PS4's libkernel does not have. ps4/app patches libc to this copy with them
# stripped, as std.patch does for std's own libc. Made from the registry source of the version in ps4/app/Cargo.lock.
LV=$(awk '/^name = "libc"$/{getline; gsub(/version = |"/, ""); print; exit}' "$ROOT/ps4/app/Cargo.lock")
LC="$ROOT/build-ps4/libc-ps4"
if [ ! -f "$LC/.patched-$LV" ]; then
	LS=$(ls -d "$T"/cargo/registry/src/*/"libc-$LV" 2>/dev/null | head -1)
	if [ -z "$LS" ]; then
		(cd "$ROOT" && cargo fetch --manifest-path ps4/app/Cargo.toml)
		LS=$(ls -d "$T"/cargo/registry/src/*/"libc-$LV" | head -1)
	fi
	rm -rf "$LC"
	cp -r "$LS" "$LC"
	grep -rlZ '@FBSD_' "$LC/src" | xargs -0 sed -i -E 's/"([a-z_0-9]+)@FBSD_[0-9.]+"/"\1"/g'
	touch "$LC/.patched-$LV"
fi

# std and libc name libraries the PS4 does not have (FreeBSD's libexecinfo, libkvm, ...); empty archives satisfy the linker.
# libpthread is shadowed on purpose: the OpenOrbis one is musl's, but threads must be libkernel's (FreeBSD ABI, as std expects).
# libgcc_s stands in for the unwinder, which is the `unwinding` crate in ps4/shim (not OpenOrbis's unoptimised libunwind.a).
mkdir -p "$OUT" "$TD/stublibs"
for l in gcc_s execinfo pthread kvm procstat memstat devstat; do
	printf '!<arch>\n' > "$TD/stublibs/lib$l.a"
done

# libkernel goes first, before the objects: every libc/pthread call then binds to the console's FreeBSD-ABI functions, and the
# musl archive only fills in what libkernel lacks (malloc, string functions, ...).
W() { cygpath -m "$1" 2>/dev/null || echo "$1"; }
US=$(printf '\037')
RF="-C${US}link-arg=-m${US}-C${US}link-arg=elf_x86_64${US}-C${US}link-arg=--script=$(W "$OO/link.x")${US}-C${US}link-arg=--eh-frame-hdr"
RF="$RF${US}-C${US}link-arg=-L$(W "$TD/stublibs")${US}-C${US}link-arg=-L$(W "$OO/lib")${US}-C${US}link-arg=-lkernel"
RF="$RF${US}-C${US}link-arg=$(W "$OO/lib/crt1.o")${US}-C${US}link-arg=-lc${US}-C${US}link-arg=-lSceNet${US}-C${US}link-arg=-lSceSystemService${US}-C${US}link-arg=-lSceVideoOut"
# The GPU test imports Piglet (the system OpenGL ES 2 library) and WebKit's precompiled shader table, as OpenOrbis's sample does.
if [ "$NAME" = gpu ]; then
	RF="$RF${US}-C${US}link-arg=-lScePigletv2VSH${US}-C${US}link-arg=-lScePrecompiledShaders"
fi
# The GNM test: the native graphics driver, and its shaders compiled from LLVM IR (ps4/gnm/shaders/*.ll) by the toolchain's llc for
# the PS4 GPU (GCN gfx701): <name>.bin is the machine code, <name>.cfg the register values llc worked out (address value).
if [ "$NAME" = gnm ]; then
	RF="$RF${US}-C${US}link-arg=-lSceGnmDriver"
	LB="$T/rustup/toolchains/1.99.0-x86_64-pc-windows-gnu/lib/rustlib/x86_64-pc-windows-gnu/bin"
	SH="$OUT/shaders"
	mkdir -p "$SH"
	for s in "$ROOT"/ps4/gnm/shaders/*.ll; do
		b=$(basename "$s" .ll)
		"$LB/llc.exe" -mtriple=amdgcn-- -mcpu=gfx701 -filetype=obj "$s" -o "$SH/$b.o"
		"$LB/llvm-objcopy.exe" -O binary --only-section=.text "$SH/$b.o" "$SH/$b.bin"
		"$LB/llc.exe" -mtriple=amdgcn-- -mcpu=gfx701 "$s" -o - | awk '/AMDGPU[.]config/{c=1;next} /[.]section/{c=0} c&&/[.]long/{print $2}' | paste -d" " - - > "$SH/$b.cfg"
	done
	export RAC_GNM_SHADERS="$(W "$SH")"
fi
# GNM test 2: the GNM layer (ps4/gnmlib) and the radv-compiled WGSL shaders of ps4/gnmtest/shaders, made beforehand by
#   sh ps4/tools/shaders.sh ps4/gnmtest/shaders/test.wgsl build-ps4/rsh/gnmtest "bg_vs bg_fs" "cube_vs cube_fs"
if [ "$NAME" = gnmtest ]; then
	RF="$RF${US}-C${US}link-arg=-lSceGnmDriver"
	[ -f "$ROOT/build-ps4/rsh/gnmtest/test.bg_vs.rsh" ] || { echo "no shader cache: run ps4/tools/shaders.sh first (see ps4/gnmtest/Cargo.toml)"; exit 1; }
	export RAC_PS4_RSH="$(W "$ROOT/build-ps4/rsh/gnmtest")"
fi
# The wgpu test (step 4): wgpu through the GNM back end (rac-rs/vendor/wgpu-hal/src/gnm), the native graphics driver.
if [ "$NAME" = wgputest ]; then
	RF="$RF${US}-C${US}link-arg=-lSceGnmDriver"
fi
# The real game (step 5): rac_game + the recompiled code on the GNM back end, the same driver; the controller (retail/ps4pad.rs,
# scePad for the user from the user service) and the sound (audio.rs, sceAudioOut).
if [ "$NAME" = app ]; then
	RF="$RF${US}-C${US}link-arg=-lSceGnmDriver${US}-C${US}link-arg=-lScePad${US}-C${US}link-arg=-lSceUserService${US}-C${US}link-arg=-lSceAudioOut"
fi
# The PS4's FreeBSD is pre-12: 32-bit inode and device numbers, 16-bit link counts.
RF="$RF${US}--cfg${US}libc_unstable_freebsd_version=\"11\""
# Profile-guided optimisation for the app, as the PC release has it (setup.ps1): the PC profile (generated/pgo, made from the
# release's pgo/<serial>.profdata.gz) retargeted to this build's crate hashes by ps4/tools/pgo_retarget.py. The hashes come from
# the previous app.elf, so the first build of a fresh checkout has none and the next one does. PS4_PGO=0 turns it off. The retargeted
# profile is disc-derived: it stays in build-ps4/pgo and is never committed.
if [ "$NAME" = app ] && [ "${PS4_PGO:-1}" != 0 ]; then
	PGO_SRC=$(ls "$ROOT"/generated/pgo/*.profdata 2>/dev/null | head -1)
	PGO_OUT="$ROOT/build-ps4/pgo/ps4.profdata"
	if [ -n "$PGO_SRC" ] && [ -f "$OUT/app.elf" ] && [ -f "$ROOT/build/release/rac-rs.exe" ]; then
		if [ ! -f "$PGO_OUT" ] || [ "$OUT/app.elf" -nt "$PGO_OUT" ] || [ "$PGO_SRC" -nt "$PGO_OUT" ]; then
			mkdir -p "$ROOT/build-ps4/pgo"
			LB="$T/rustup/toolchains/1.99.0-x86_64-pc-windows-gnu/lib/rustlib/x86_64-pc-windows-gnu/bin"
			"$T/python/python.exe" -I "$ROOT/ps4/tools/pgo_retarget.py" "$(W "$LB/llvm-nm.exe")" "$(W "$LB/llvm-profdata.exe")" \
				"$(W "$ROOT/build/release/rac-rs.exe")" "$(W "$OUT/app.elf")" "$(W "$PGO_SRC")" "$(W "$PGO_OUT")" || rm -f "$PGO_OUT"
		fi
		if [ -f "$PGO_OUT" ]; then
			RF="$RF${US}-Cprofile-use=$(W "$PGO_OUT")${US}-Cllvm-args=-no-pgo-warn-mismatch${US}-Cllvm-args=-pgso=false"
			echo "ps4/build.sh: PGO on ($PGO_OUT)"
		fi
	else
		echo "ps4/build.sh: PGO off (needs generated/pgo, build/release/rac-rs.exe and a previous build-ps4/app/app.elf)"
	fi
fi
export CARGO_ENCODED_RUSTFLAGS="$RF"

cd "$ROOT"
cargo build --release --manifest-path "ps4/$NAME/Cargo.toml" --target-dir "$TD" \
	--target ps4/x86_64-rac-ps4.json -Z build-std=std,panic_unwind -Z json-target-spec \
	--config generated/build_profile.toml "$@"

ELF="$TD/x86_64-rac-ps4/release/rac-ps4-$NAME"
cp "$ELF" "$OUT/$NAME.elf"
export OO_PS4_TOOLCHAIN="$(W "$OO")"
"$OO/bin/windows/create-fself.exe" -in="$(W "$OUT/$NAME.elf")" -out="$(W "$OUT/$NAME.oelf")" --eboot "$(W "$OUT/eboot.bin")" --paid 0x3800000000000011
ls -la "$OUT/eboot.bin"
