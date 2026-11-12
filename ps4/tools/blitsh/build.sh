#!/bin/sh
# PS4 port: compiles the GNM back end's blit shaders (blit_vs.ll, fill_ps.ll, copy_ps.ll) with the private toolchain's llc for
# the PS4 GPU (GCN gfx701) and prints each program's machine code as the words rac-rs/vendor/wgpu-hal/src/gnm/blit.rs embeds
# (and the .s listing with llc's register values next to each .ll). Rerun after changing a .ll and paste the words into blit.rs.
#   sh ps4/tools/blitsh/build.sh
set -e
D=$(cd "$(dirname "$0")" && pwd)
LB="$D/../../../toolchain/rustup/toolchains/1.99.0-x86_64-pc-windows-gnu/lib/rustlib/x86_64-pc-windows-gnu/bin"
for s in blit_vs fill_ps copy_ps; do
	"$LB/llc.exe" -mtriple=amdgcn-- -mcpu=gfx701 "$D/$s.ll" -o "$D/$s.s"
	"$LB/llc.exe" -mtriple=amdgcn-- -mcpu=gfx701 -filetype=obj "$D/$s.ll" -o "$D/$s.o"
	"$LB/llvm-objcopy.exe" -O binary --only-section=.text "$D/$s.o" "$D/$s.bin"
	echo "$s: $(od -An -tx4 -v "$D/$s.bin" | tr -s ' \n' ' ')"
	rm -f "$D/$s.o" "$D/$s.bin"
done
