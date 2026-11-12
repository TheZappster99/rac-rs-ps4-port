//! The back end's own draws for render-pass clears and resolves (`RAC_GNM_BLIT`), instead of CP DMA.
//!
//! Why: the console's GPU profile (round 10, `RAC_GNM_GPUPROF`) showed CP DMA running at about 2.7 GB/s (the pass "post fog
//! cover clear" fills 2 MB in 0.77 ms), and most of a level frame's GPU time was CP DMA: the three 2048x2048 sun shadow depth
//! clears (~19 ms), the Rgba32Float + depth clear of "water positions" (~15 ms), the resolve copies between passes (~15 ms)
//! and the level pass's own clears (~7 ms) of a ~100 ms frame. The colour and depth units write that memory more than a
//! hundred times faster, so a clear is one triangle over the target, and a resolve (MSAA targets are drawn single-sampled
//! here, so a resolve is a plain copy) is one triangle whose pixel shader loads the source texel at its own position.
//!
//! The shaders are LLVM IR in ps4/tools/blitsh (blit_vs.ll, fill_ps.ll, copy_ps.ll), compiled by the private toolchain's llc
//! (-mtriple=amdgcn-- -mcpu=gfx701; ps4/tools/blitsh/build.sh) like the console-proven triangle test (ps4/gnm). llc leaves
//! RSRC2's USER_SGPR at 0, so the counts are set here. Depth clears use the no-op pixel shader of depth-only pipelines.
//!
//! `RAC_GNM_BLIT=clear|resolve|all` (default: neither, CP DMA as before; a value of 0 = neither). Only single-layer targets
//! of float / unorm / snorm formats (and depth) take the draw; integer, sRGB and layered ones keep CP DMA. Stencil clears
//! keep CP DMA (1 byte a texel).
//! `RAC_GNM_BLIT_CHECK=frames` (console diagnostics): in those frames each blit is followed by a barrier and CP DMA copies of
//! the target's middle row (and, for a resolve, the source's), compared at present: "GNM blit check ... N of M match".

use alloc::{string::String, vec::Vec};

/// Vertex shader (blit_vs.ll): vertices 0-2 = one triangle over the whole target, user data 0 = z, one parameter exported.
pub const VS: [u32; 14] = [
	0x7e0202ff, 0x40400000, 0x7d840081, 0x000402f3, 0x7d840082, 0x000002f3, 0x7e0602f2, 0x7e080200, 0x7e020280, 0xf80008cf, 0x03040002, 0xf800020f,
	0x01010101, 0xbf810000,
];
/// Pixel shader (fill_ps.ll): user data 0-3 = the colour, to target 0 as 32-bit floats.
pub const FILL_PS: [u32; 7] = [0x7e000200, 0x7e020201, 0x7e040202, 0x7e060203, 0xf800180f, 0x03020100, 0xbf810000];
/// Pixel shader (copy_ps.ll): user data 0-7 = the source's image descriptor, image_load at the pixel's position, to target 0.
pub const COPY_PS: [u32; 8] = [0x7e000f02, 0x7e020f03, 0xf0001f00, 0x00000000, 0xbf8c0f70, 0xf800180f, 0x03020100, 0xbf810000];

/// Buffer clears of at least this many bytes take a draw (`CommandEncoder::blit_fill_buffer`), smaller ones CP DMA.
pub const BUFFER_FILL_MIN: u64 = 256 << 10;
/// The width in R32 texels of the colour target a buffer clear draws (8 KB rows; a multiple of the 64 texels a linear target's
/// pitch needs).
pub const BUFFER_FILL_WIDTH: u32 = 2048;

/// Offsets of the three programs in `Shared::blit` (each 256-byte aligned, as the hardware wants).
pub const VS_AT: usize = 0;
pub const FILL_AT: usize = 256;
pub const COPY_AT: usize = 512;
pub const CODE_BYTES: usize = 768;

/// Writes the three programs into `words` (the `CODE_BYTES` block).
pub fn write_code(words: &mut [u32]) {
	words.fill(0);
	words[VS_AT / 4..VS_AT / 4 + VS.len()].copy_from_slice(&VS);
	words[FILL_AT / 4..FILL_AT / 4 + FILL_PS.len()].copy_from_slice(&FILL_PS);
	words[COPY_AT / 4..COPY_AT / 4 + COPY_PS.len()].copy_from_slice(&COPY_PS);
}

/// SPI_SHADER_COL_FORMAT 32_ABGR for target 0: full 32-bit values, which any float / unorm / snorm target takes.
const COL_32_ABGR: u32 = 9;

/// sceGnmSetVsShader's structure (see gnmlib `Shader::vs_regs`): RSRC1 from llc (8 VGPRs), RSRC2 USER_SGPR = 1, one
/// parameter (VS_EXPORT_COUNT 0), position 4 components.
pub fn vs_regs(addr: u64) -> [u32; 7] {
	[(addr >> 8) as u32, (addr >> 40) as u32, 1, 1 << 1, 0, 4, 0]
}

/// sceGnmSetPsShader350's structure for the fill shader: 4 user SGPRs, PERSP_SAMPLE as the one enabled input (llc's
/// choice: the hardware needs one), no interpolated inputs, target 0 written.
pub fn fill_regs(addr: u64) -> [u32; 12] {
	[(addr >> 8) as u32, (addr >> 40) as u32, 0, 4 << 1, 0, COL_32_ABGR, 1, 1, 0, 0, 0, 0xf]
}

/// The copy shader: 8 user SGPRs, PERSP_SAMPLE + POS_X + POS_Y (0x301, llc's), target 0 written. RSRC1 asks for 16 SGPRs and 8
/// VGPRs: llc's RSRC1 (0 = 8 SGPRs) counts only the 8 user SGPRs, but the hardware also writes PRIM_MASK into the SGPR after
/// them (s8), outside an 8-SGPR allocation; that corrupted a neighbouring wave's descriptor and left blue specks on every
/// resolve (round 14 run 3: ~3% of each checked row wrong). The fill shader's 4 user SGPRs + PRIM_MASK fit in 8.
pub fn copy_regs(addr: u64) -> [u32; 12] {
	[(addr >> 8) as u32, (addr >> 40) as u32, 1 << 6 | 1, 8 << 1, 0, COL_32_ABGR, 0x301, 0x301, 0, 0, 0, 0xf]
}

/// Which jobs take the draw: bit 0 clears, bit 1 resolves.
pub fn mode() -> u32 {
	static M: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
	*M.get_or_init(|| {
		let m = match std::env::var("RAC_GNM_BLIT").unwrap_or_default().trim() {
			"clear" => 1,
			"resolve" => 2,
			"all" | "1" => 3,
			_ => 0,
		};
		if m != 0 {
			log::info!("GNM: RAC_GNM_BLIT: {} by draws", ["", "clears", "resolves", "clears and resolves"][m as usize]);
		}
		m
	})
}

/// `RAC_GNM_BLIT_CHECK=frames` (a `pipe::Frames` list): whether frame `n` checks its blits.
pub fn checking(n: u64) -> bool {
	static F: std::sync::OnceLock<super::pipe::Frames> = std::sync::OnceLock::new();
	F.get_or_init(|| super::pipe::Frames::env("RAC_GNM_BLIT_CHECK")).has(n)
}

/// Whether a colour view can take a draw (clear or copy target): a colour format of one layer, not integer (blit exports
/// floats), not sRGB (a copy would round-trip through linear).
pub fn colour_ok(v: &super::TextureView) -> bool {
	let f = &v.fmt;
	f.cb_format != 0 && f.depth.is_none() && !matches!(f.cb_number, 4 | 5 | 6) && v.layers <= 1
}

/// A depth view that can take a draw clear: a depth plane, one layer.
pub fn depth_ok(v: &super::TextureView) -> bool {
	v.fmt.depth.is_some_and(|(z, _)| z != 0) && v.layers <= 1
}

/// A pending check: what, the slot of the row read back, and either the expected texel (clear) or the source row's slot.
pub struct Check {
	pub what: String,
	pub got: u64,
	pub want: Want,
	pub bytes: usize,
}

pub enum Want {
	Texel(Vec<u8>),
	Row(u64),
}

/// Compares the finished frame's checks (`Shared::blit_checks`) and prints one line each. The rows were copied by the CP of
/// a frame the GPU has finished.
pub fn print_checks(checks: Vec<Check>) {
	for c in checks {
		// SAFETY: slots in the snapshot block, written by the CP of a finished frame (`Shared::snap_slot`).
		let got = unsafe { core::slice::from_raw_parts(c.got as *const u8, c.bytes) };
		let (ok, n, first_bad) = match &c.want {
			Want::Texel(t) => {
				let n = c.bytes / t.len().max(1);
				let bad: Vec<usize> = (0..n).filter(|&i| &got[i * t.len()..(i + 1) * t.len()] != t.as_slice()).collect();
				(n - bad.len(), n, bad.first().map(|&i| (i, got[i * t.len()..(i + 1) * t.len()].to_vec(), t.clone())))
			}
			Want::Row(src) => {
				// SAFETY: as above.
				let want = unsafe { core::slice::from_raw_parts(*src as *const u8, c.bytes) };
				let n = c.bytes / 4;
				let bad: Vec<usize> = (0..n).filter(|&i| got[i * 4..i * 4 + 4] != want[i * 4..i * 4 + 4]).collect();
				(n - bad.len(), n, bad.first().map(|&i| (i, got[i * 4..i * 4 + 4].to_vec(), want[i * 4..i * 4 + 4].to_vec())))
			}
		};
		match first_bad {
			None => std::eprintln!("GNM blit check {}: {ok} of {n} match", c.what),
			Some((i, g, w)) => std::eprintln!("GNM blit check {}: {ok} of {n} match; first difference at {i}: got {g:02x?}, want {w:02x?}", c.what),
		}
	}
}
