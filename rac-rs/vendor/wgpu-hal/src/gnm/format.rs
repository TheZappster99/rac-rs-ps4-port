//! wgpu texture formats as GFX7 sees them: image descriptor formats (T#), colour buffer formats (CB_COLOR*_INFO) and depth
//! formats (DB_Z_INFO). Values: AMD Sea Islands registers / Mesa sid.h (IMG_DATA_FORMAT_*, IMG_NUM_FORMAT_*, COLOR_*,
//! NUMBER_*, SWAP_*, Z_*).

use rac_ps4_gnm::desc::{SEL_0, SEL_1, SEL_W, SEL_X, SEL_Y, SEL_Z};
use wgt::TextureFormat as F;

/// How one format is stored and read.
#[derive(Clone, Copy, Debug)]
pub struct Fmt {
	/// Bytes per texel (colour formats; 4 for the depth plane of depth formats).
	pub bytes: u32,
	pub img_data: u32,
	pub img_num: u32,
	/// T# component selects (x, y, z, w).
	pub dst_sel: [u32; 4],
	/// Colour buffer format (0 = cannot be a colour target).
	pub cb_format: u32,
	pub cb_number: u32,
	pub cb_swap: u32,
	/// Depth: DB_Z_INFO FORMAT (1 = Z_16, 3 = Z_32_FLOAT), and whether a stencil plane (1 byte per texel) follows.
	pub depth: Option<(u32, bool)>,
}

const RGBA: [u32; 4] = [SEL_X, SEL_Y, SEL_Z, SEL_W];
const BGRA: [u32; 4] = [SEL_Z, SEL_Y, SEL_X, SEL_W];
const R: [u32; 4] = [SEL_X, SEL_0, SEL_0, SEL_1];
const RG: [u32; 4] = [SEL_X, SEL_Y, SEL_0, SEL_1];
const RGB1: [u32; 4] = [SEL_X, SEL_Y, SEL_Z, SEL_1];

// Number types (CB) and number formats (images).
const UNORM: (u32, u32) = (0, 0);
const SNORM: (u32, u32) = (1, 1);
const UINT: (u32, u32) = (4, 4);
const SINT: (u32, u32) = (5, 5);
const FLOAT: (u32, u32) = (7, 7);
const SRGB: (u32, u32) = (9, 6);

const fn c(bytes: u32, data: u32, (num, cbn): (u32, u32), sel: [u32; 4], swap: u32) -> Fmt {
	Fmt { bytes, img_data: data, img_num: num, dst_sel: sel, cb_format: data, cb_number: cbn, cb_swap: swap, depth: None }
}

/// The format's layout, or `None` if this back end does not support it.
pub fn of(f: F) -> Option<Fmt> {
	Some(match f {
		F::R8Unorm => c(1, 1, UNORM, R, 0),
		F::R8Snorm => c(1, 1, SNORM, R, 0),
		F::R8Uint => c(1, 1, UINT, R, 0),
		F::R8Sint => c(1, 1, SINT, R, 0),
		F::R16Uint => c(2, 2, UINT, R, 0),
		F::R16Sint => c(2, 2, SINT, R, 0),
		F::R16Unorm => c(2, 2, UNORM, R, 0),
		F::R16Float => c(2, 2, FLOAT, R, 0),
		F::Rg8Unorm => c(2, 3, UNORM, RG, 0),
		F::Rg8Snorm => c(2, 3, SNORM, RG, 0),
		F::Rg8Uint => c(2, 3, UINT, RG, 0),
		F::R32Uint => c(4, 4, UINT, R, 0),
		F::R32Sint => c(4, 4, SINT, R, 0),
		F::R32Float => c(4, 4, FLOAT, R, 0),
		F::Rg16Uint => c(4, 5, UINT, RG, 0),
		F::Rg16Sint => c(4, 5, SINT, RG, 0),
		F::Rg16Unorm => c(4, 5, UNORM, RG, 0),
		F::Rg16Float => c(4, 5, FLOAT, RG, 0),
		F::Rgba8Unorm => c(4, 10, UNORM, RGBA, 0),
		F::Rgba8UnormSrgb => c(4, 10, SRGB, RGBA, 0),
		F::Rgba8Snorm => c(4, 10, SNORM, RGBA, 0),
		F::Rgba8Uint => c(4, 10, UINT, RGBA, 0),
		F::Rgba8Sint => c(4, 10, SINT, RGBA, 0),
		// Memory order B G R A: the colour buffer swaps (ALT, as the console-proven display target), the T# swizzles.
		F::Bgra8Unorm => c(4, 10, UNORM, BGRA, 1),
		F::Bgra8UnormSrgb => c(4, 10, SRGB, BGRA, 1),
		F::Rgb10a2Unorm => c(4, 9, UNORM, RGBA, 0),
		F::Rgb10a2Uint => c(4, 9, UINT, RGBA, 0),
		// 11-11-10 float, R in the low bits: the hardware's 10_11_11 names fields from the top.
		F::Rg11b10Ufloat => c(4, 6, FLOAT, RGB1, 0),
		F::Rg32Uint => c(8, 11, UINT, RG, 0),
		F::Rg32Sint => c(8, 11, SINT, RG, 0),
		F::Rg32Float => c(8, 11, FLOAT, RG, 0),
		F::Rgba16Uint => c(8, 12, UINT, RGBA, 0),
		F::Rgba16Sint => c(8, 12, SINT, RGBA, 0),
		F::Rgba16Unorm => c(8, 12, UNORM, RGBA, 0),
		F::Rgba16Float => c(8, 12, FLOAT, RGBA, 0),
		F::Rgba32Uint => c(16, 14, UINT, RGBA, 0),
		F::Rgba32Sint => c(16, 14, SINT, RGBA, 0),
		F::Rgba32Float => c(16, 14, FLOAT, RGBA, 0),
		// Depth: sampled as R (32-bit float or 16-bit unorm); 24-bit depth is stored as 32-bit float, as radv does on GFX7.
		F::Depth16Unorm => Fmt { depth: Some((1, false)), ..c(2, 2, UNORM, R, 0) },
		F::Depth32Float | F::Depth24Plus => Fmt { depth: Some((3, false)), ..c(4, 4, FLOAT, R, 0) },
		F::Depth32FloatStencil8 | F::Depth24PlusStencil8 => Fmt { depth: Some((3, true)), ..c(4, 4, FLOAT, R, 0) },
		F::Stencil8 => Fmt { depth: Some((0, true)), ..c(1, 1, UINT, R, 0) },
		_ => return None,
	})
	.map(|mut f| {
		if f.depth.is_some() {
			f.cb_format = 0;
		}
		f
	})
}

/// Level `l`'s pitch in texels in a mipmapped colour texture (T# POW2_PAD set), as the PS4's texture unit computes it
/// (measured on the console by wgputest's mip address probe, runs 5-6): from the base pitch (not the width), every level a
/// power of two, aligned to max(8, 64 bytes) texels (addrlib SI linear aligned), and widened until pitch x rows covers at
/// least 64 texels (one micro tile), which only bites at the smallest levels (R32 2x2: pitch 32; 16-byte 4x4: pitch 16).
/// `rows` is the level's power-of-two padded height.
pub fn mip_pitch(width: u32, bytes: u32, l: u32, rows: u32) -> u32 {
	let align = (64 / bytes.max(1)).max(8);
	let base = width.max(1).next_power_of_two().max(align);
	(base >> l).max(1).next_power_of_two().max(align).max(64 / rows.max(1))
}

/// Linear-aligned layout rule (GFX7 ARRAY_LINEAR_ALIGNED): rows padded to 64 texels, and to at least 256 bytes... as the
/// texture unit and colour buffer expect. Depth targets use 1D tiling (8x8 tiles), whose padding is 8 texels.
pub fn pitch_texels(width: u32, bytes: u32, depth: bool) -> u32 {
	if depth {
		width.next_multiple_of(8)
	} else {
		let min = (256 / bytes.max(1)).max(64);
		width.next_multiple_of(min)
	}
}
