#!/bin/sh
# PS4 port: builds the PS4 shader cache from a pipeline capture (PS4_GPU_DESIGN.md, "The shader cache key and the capture").
#   sh ps4/tools/shcache.sh [CAPTURE_DIR] [OUT_FILE]      (defaults: build-ps4/capture, build-ps4/ps4shaders.bin)
# CAPTURE_DIR comes from the PC game run with RAC_PS4_CAPTURE=DIR (with the Ray tracing mod off: the PS4 has none). Then:
#   vkrec (WSL Kali, Mesa radv on a fake GFX7 device): every record -> radv binaries + set layouts in build-ps4/capbin;
#   radvbin: binaries -> .rsh in build-ps4/caprsh;  shcache: checks every pipeline and packs OUT_FILE.
# Needs WSL Kali with mesa-vulkan-drivers, mesa-drm-shim, libvulkan-dev, gcc. Everything it writes stays in build-ps4/
# (gitignored): the capture holds shaders made from rac-rs's own WGSL, but keep it with the rest of the PS4 build.
set -e
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
CAP=${1:-$ROOT/build-ps4/capture}
OUT=${2:-$ROOT/build-ps4/ps4shaders.bin}
T="$ROOT/toolchain"
export RUSTUP_HOME="$T/rustup" CARGO_HOME="$T/cargo" RUSTUP_TOOLCHAIN=1.99.0-x86_64-pc-windows-gnu
export PATH="$T/cargo/bin:$T/rustup/toolchains/1.99.0-x86_64-pc-windows-gnu/lib/rustlib/x86_64-pc-windows-gnu/bin/self-contained:$PATH"
cargo build -q --release --manifest-path "$ROOT/ps4/tools/radvbin/Cargo.toml" --target-dir "$ROOT/build-ps4/tools"
cargo build -q --release --manifest-path "$ROOT/ps4/tools/shcache/Cargo.toml" --target-dir "$ROOT/build-ps4/tools"
BIN="$ROOT/build-ps4/capbin"; RSH="$ROOT/build-ps4/caprsh"
rm -rf "$BIN" "$RSH"; mkdir -p "$BIN" "$RSH" "$ROOT/build-ps4/wsl"
# Paths as WSL sees them (/mnt/<drive>/...).
wslp() { cygpath -m "$1" | sed -E 's|^([A-Za-z]):|/mnt/\L\1|'; }
R=$(wslp "$ROOT"); C=$(wslp "$CAP"); B=$(wslp "$BIN")
cat > "$ROOT/build-ps4/wsl/vkrec.sh" <<EOS
#!/bin/sh
cd "$R"
gcc -O1 -Wall -o build-ps4/wsl/vkrec ps4/tools/vkc/vkrec.c -lvulkan || exit 255
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/radeon_icd.json LD_PRELOAD=/usr/lib/x86_64-linux-gnu/libamdgpu_noop_drm_shim.so AMDGPU_GPU_ID=bonaire
build-ps4/wsl/vkrec "$C" "$B" 2>&1 | grep -v DRI3
EOS
MSYS_NO_PATHCONV=1 wsl.exe -d kali-linux -- sh "$R/build-ps4/wsl/vkrec.sh" > "$ROOT/build-ps4/capbin.log" 2>&1 || true
tail -1 "$ROOT/build-ps4/capbin.log"
grep " FAIL " "$ROOT/build-ps4/capbin.log" || true
"$ROOT/build-ps4/tools/release/radvbin.exe" "$RSH" "$BIN"/*.bin > "$ROOT/build-ps4/caprsh.log"
"$ROOT/build-ps4/tools/release/shcache.exe" "$CAP" "$BIN" "$RSH" "$OUT"
