#!/bin/sh
# PS4 port: compiles WGSL shaders for the PS4's GPU into the GNM layer's shader-cache entries (.rsh), on this PC.
#   sh ps4/tools/shaders.sh FILE.wgsl OUTDIR "VS FS" ["VS FS" | CS ...]
# WGSL -> SPIR-V (ps4/tools/wgsl2spv, naga) -> Mesa radv on a fake GFX7 GPU in WSL Kali (ps4/tools/vkc, VK_KHR_pipeline_binary)
# -> .rsh (ps4/tools/radvbin). Pipelines are compiled for B8G8R8A8 colour targets and a D32 depth target (the PS4 layer's).
# OUTDIR/<file>.<entry>.rsh per stage; OUTDIR/radv.log has radv's final NIR of each (RADV_DEBUG=shaders), the place to read
# which user SGPR / descriptor offset a shader uses. Needs WSL Kali with mesa-vulkan-drivers, mesa-drm-shim, libvulkan-dev, gcc.
set -e
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WGSL=$1; OUT=$2; shift 2
T="$ROOT/toolchain"
export RUSTUP_HOME="$T/rustup" CARGO_HOME="$T/cargo" RUSTUP_TOOLCHAIN=1.99.0-x86_64-pc-windows-gnu
export PATH="$T/cargo/bin:$T/rustup/toolchains/1.99.0-x86_64-pc-windows-gnu/lib/rustlib/x86_64-pc-windows-gnu/bin/self-contained:$PATH"
cargo build -q --release --manifest-path "$ROOT/ps4/tools/wgsl2spv/Cargo.toml" --target-dir "$ROOT/build-ps4/tools"
cargo build -q --release --manifest-path "$ROOT/ps4/tools/radvbin/Cargo.toml" --target-dir "$ROOT/build-ps4/tools"
STEM=$(basename "$WGSL" .wgsl)
SPV="$ROOT/build-ps4/spv/$STEM"; BIN="$ROOT/build-ps4/bin/$STEM"
rm -rf "$SPV" "$BIN"; mkdir -p "$SPV" "$BIN" "$OUT"
"$ROOT/build-ps4/tools/release/wgsl2spv.exe" "$WGSL" "$SPV" > /dev/null
# Paths as WSL sees them (/mnt/<drive>/...).
wslp() { cygpath -m "$1" | sed -E 's|^([A-Za-z]):|/mnt/\L\1|'; }
R=$(wslp "$ROOT"); S=$(wslp "$SPV"); B=$(wslp "$BIN")
cat > "$ROOT/build-ps4/wsl/shaders_run.sh" <<EOS
#!/bin/sh
cd "$R"
gcc -O1 -o build-ps4/wsl/vkc ps4/tools/vkc/vkc.c -lvulkan || exit 1
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/radeon_icd.json LD_PRELOAD=/usr/lib/x86_64-linux-gnu/libamdgpu_noop_drm_shim.so AMDGPU_GPU_ID=bonaire
export VKC_DUMP="$B" VKC_COLOR=bgra8 RADV_DEBUG=shaders
: > "$B/radv.log"
for job in "\$@"; do
  set -- \$job
  if [ \$# -eq 2 ]; then build-ps4/wsl/vkc "$S/$STEM.\$1" "$S/$STEM.\$2" >> "$B/radv.log" 2>&1 || { echo "vkc failed on \$1 \$2"; tail -5 "$B/radv.log"; exit 1; }
  else build-ps4/wsl/vkc "$S/$STEM.\$1" >> "$B/radv.log" 2>&1 || { echo "vkc failed on \$1"; tail -5 "$B/radv.log"; exit 1; }; fi
done
EOS
MSYS_NO_PATHCONV=1 wsl.exe -d kali-linux -- sh "$R/build-ps4/wsl/shaders_run.sh" "$@"
# vkc names each blob after the first stage of its pipeline (.0 = that stage, .1 = the second): rename to <file>.<entry>.
for job in "$@"; do
	set -- $job
	[ -f "$BIN/$STEM.$1.0.bin" ] && mv "$BIN/$STEM.$1.0.bin" "$BIN/$STEM.$1.bin"
	[ $# -eq 2 ] && [ -f "$BIN/$STEM.$1.1.bin" ] && mv "$BIN/$STEM.$1.1.bin" "$BIN/$STEM.$2.bin"
done
"$ROOT/build-ps4/tools/release/radvbin.exe" "$OUT" "$BIN"/*.bin
cp "$BIN/radv.log" "$OUT/radv.log"
