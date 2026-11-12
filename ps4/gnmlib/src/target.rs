//! Render targets: colour target 0 and the depth target, as register writes.

use crate::desc::{TILE_DEPTH_1D_THIN, TILE_DISPLAY_LINEAR_ALIGNED};
use crate::pm4::Cmd;
use crate::regs;

/// A colour target: `addr` (256-byte aligned), `width` x `height`, `pitch` in pixels (a multiple of 64), 8-8-8-8 unorm in
/// memory order B G R A (the PS4 display buffer format A8R8G8B8: COMP_SWAP ALT), linear aligned. As in the console-proven
/// triangle test (ps4/gnm).
pub fn color_bgra8(c: &mut Cmd, addr: u64, width: u32, height: u32, pitch: u32) {
	assert!(addr % 256 == 0 && pitch % 64 == 0 && pitch >= width);
	let lo = (addr >> 8) as u32;
	let slice = pitch * height / 64 - 1;
	let _ = width;
	c.ctx(regs::CB_COLOR0_BASE, &[
		lo,                                  // CB_COLOR0_BASE
		pitch / 8 - 1,                       // CB_COLOR0_PITCH (TILE_MAX)
		slice,                               // CB_COLOR0_SLICE (TILE_MAX)
		0,                                   // CB_COLOR0_VIEW
		(0x0a << 2) | (1 << 11) | (1 << 15), // CB_COLOR0_INFO: FORMAT 8_8_8_8, COMP_SWAP ALT, BLEND_CLAMP
		TILE_DISPLAY_LINEAR_ALIGNED | TILE_DISPLAY_LINEAR_ALIGNED << 5, // CB_COLOR0_ATTRIB: tile and FMASK tile mode index
		0,                                   // (reserved)
		lo,                                  // CB_COLOR0_CMASK (unused: no fast clear)
		0,                                   // CB_COLOR0_CMASK_SLICE
		lo,                                  // CB_COLOR0_FMASK (unused: no MSAA)
		slice,                               // CB_COLOR0_FMASK_SLICE
		0,
		0, // CB_COLOR0_CLEAR_WORD0/1
	]);
	c.ctx(regs::CB_TARGET_MASK, &[0xf]);
	// MODE normal, ROP3 copy.
	c.ctx(regs::CB_COLOR_CONTROL, &[(1 << 4) | (0xcc << 16)]);
}

/// Bytes of a 32-bit float depth target: 1D tiled in 8x8 tiles, so width and height round up to 8.
pub fn depth_bytes(width: u32, height: u32) -> usize {
	(width.div_ceil(8) * 8 * height.div_ceil(8) * 8 * 4) as usize
}

/// The depth target: 32-bit float, 1D-tiled thin (PS4 tile mode 5; depth cannot be linear on this GPU), no stencil, no HTILE
/// (so no fast clears: a frame clears depth by drawing with depth test ALWAYS). `addr` 256-byte aligned.
pub fn depth_z32f(c: &mut Cmd, addr: u64, width: u32, height: u32) {
	assert!(addr % 256 == 0);
	let (pw, ph) = (width.div_ceil(8), height.div_ceil(8));
	let a = (addr >> 8) as u32;
	// On GFX7 the depth block takes its layout from DB_DEPTH_INFO, not from DB_Z_INFO.TILE_MODE_INDEX (Mesa radeonsi only
	// sets the index on GFX6). Left at the default state's value, the first frame drew (the buffer was all zeros) and the next
	// one hung the GPU on the console. ARRAY_MODE 2 = 1D tiled thin; pipe config 12 = P8_32x32_16x16 (unused by 1D tiling).
	c.ctx(regs::DB_DEPTH_INFO, &[
		2 << 4 | 12 << 8,             // DB_DEPTH_INFO: ARRAY_MODE, PIPE_CONFIG
		3 | TILE_DEPTH_1D_THIN << 20, // DB_Z_INFO: FORMAT Z_32_FLOAT, TILE_MODE_INDEX (GFX6 only; kept for tools)
		0,                            // DB_STENCIL_INFO: no stencil
		a,                            // DB_Z_READ_BASE
		a,                            // DB_STENCIL_READ_BASE
		a,                            // DB_Z_WRITE_BASE
		a,                            // DB_STENCIL_WRITE_BASE
		(pw - 1) | (ph - 1) << 11,    // DB_DEPTH_SIZE: PITCH_TILE_MAX, HEIGHT_TILE_MAX
		pw * ph - 1,                  // DB_DEPTH_SLICE: SLICE_TILE_MAX
	]);
	c.ctx(regs::DB_DEPTH_VIEW, &[0]);
	c.ctx(regs::DB_HTILE_DATA_BASE, &[0]);
	c.ctx(regs::DB_RENDER_CONTROL, &[0]);
}

/// No depth target.
pub fn depth_none(c: &mut Cmd) {
	c.ctx(regs::DB_Z_INFO, &[0, 0]);
	c.ctx(regs::DB_RENDER_CONTROL, &[0]);
	c.ctx(regs::DB_DEPTH_CONTROL, &[0]);
}

/// Scissors and viewport over the whole `width` x `height` target, Vulkan conventions (what radv-compiled shaders produce):
/// NDC y = -1 at the top, z in 0..1 maps straight to depth.
pub fn viewport(c: &mut Cmd, width: u32, height: u32) {
	let br = (height << 16) | width;
	c.ctx(regs::PA_SC_SCREEN_SCISSOR_TL, &[0, br]);
	c.ctx(regs::PA_SC_WINDOW_OFFSET, &[0, 0x8000_0000, br]); // + PA_SC_WINDOW_SCISSOR_TL/BR
	c.ctx(regs::PA_SC_GENERIC_SCISSOR_TL, &[0x8000_0000, br]);
	c.ctx(regs::PA_SC_VPORT_SCISSOR_0_TL, &[0x8000_0000, br]);
	c.ctx(regs::PA_SC_VPORT_ZMIN_0, &[0f32.to_bits(), 1f32.to_bits()]);
	let (hw, hh) = (width as f32 / 2.0, height as f32 / 2.0);
	// XSCALE, XOFFSET, YSCALE, YOFFSET, ZSCALE, ZOFFSET.
	c.ctx(regs::PA_CL_VPORT_XSCALE, &[hw.to_bits(), hw.to_bits(), hh.to_bits(), hh.to_bits(), 1f32.to_bits(), 0f32.to_bits()]);
	// Viewport scale and offset on, W is 1/W.
	c.ctx(regs::PA_CL_VTE_CNTL, &[0x43f]);
	// DX_CLIP_SPACE_DEF: clip z to 0..w (Vulkan / D3D), not -w..w.
	c.ctx(regs::PA_CL_CLIP_CNTL, &[1 << 19]);
	c.ctx(regs::PA_SC_AA_CONFIG, &[0]);
	c.ctx(regs::PA_SC_AA_MASK_X0Y0_X1Y0, &[0xffff_ffff, 0xffff_ffff]);
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn depth_size_fields() {
		let mut b = [0u32; 64];
		let mut c = Cmd::new(&mut b);
		depth_z32f(&mut c, 0x1000, 1920, 1080);
		let w = c.words();
		// header, register index, then the 8 values.
		assert_eq!(w[1], (0x2803c - 0x28000) / 4);
		assert_eq!((w[2] >> 4) & 0xf, 2);
		assert_eq!(w[3] & 3, 3);
		assert_eq!((w[3] >> 20) & 7, 5);
		assert_eq!(w[9], 239 | 134 << 11);
		assert_eq!(w[10], 240 * 135 - 1);
		assert_eq!(depth_bytes(1920, 1080), 1920 * 1080 * 4);
	}
}
