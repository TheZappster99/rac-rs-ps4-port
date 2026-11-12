//! GFX7 resource descriptors, as shaders read them from descriptor sets (radv's layout puts them in memory verbatim).
//! Field layouts: AMD Sea Islands ISA "Buffer / Image / Sampler resource", Mesa sid.h (SQ_BUF_RSRC_WORD*, SQ_IMG_RSRC_WORD*,
//! SQ_IMG_SAMP_WORD*), shadPS4 resource.h (the PS4's own use).

/// Component selects (SQ_SEL_*).
pub const SEL_0: u32 = 0;
pub const SEL_1: u32 = 1;
pub const SEL_X: u32 = 4;
pub const SEL_Y: u32 = 5;
pub const SEL_Z: u32 = 6;
pub const SEL_W: u32 = 7;

/// Buffer data / number formats (BUF_DATA_FORMAT_*, BUF_NUM_FORMAT_*).
pub const BUF_DATA_FORMAT_32: u32 = 4;
pub const BUF_NUM_FORMAT_FLOAT: u32 = 7;

/// A buffer resource (V#): `addr` (48-bit GPU address), `stride` bytes per record (0 = raw bytes), `num_records` (bytes when
/// stride is 0, else records). Data format 32 / float, XYZW selects: what radv writes for uniform, storage and vertex buffers
/// (shaders do untyped loads; the format only has to be valid).
pub fn buffer(addr: u64, stride: u32, num_records: u32) -> [u32; 4] {
	assert!(stride < (1 << 14));
	[
		addr as u32,
		((addr >> 32) as u32 & 0xffff) | stride << 16,
		num_records,
		SEL_X | SEL_Y << 3 | SEL_Z << 6 | SEL_W << 9 | BUF_NUM_FORMAT_FLOAT << 12 | BUF_DATA_FORMAT_32 << 15,
	]
}

/// Image data formats (IMG_DATA_FORMAT_*) and number formats (IMG_NUM_FORMAT_*).
pub const IMG_DATA_FORMAT_8_8_8_8: u32 = 10;
pub const IMG_DATA_FORMAT_32: u32 = 4;
pub const IMG_NUM_FORMAT_UNORM: u32 = 0;
pub const IMG_NUM_FORMAT_FLOAT: u32 = 7;
pub const IMG_NUM_FORMAT_SRGB: u32 = 9;
/// SQ_RSRC_IMG_2D.
pub const IMG_TYPE_2D: u32 = 9;
/// PS4 tile mode indices (GB_TILE_MODE table; shadPS4 tiling.h).
pub const TILE_DEPTH_1D_THIN: u32 = 5;
pub const TILE_DISPLAY_LINEAR_ALIGNED: u32 = 8;
pub const TILE_THIN_1D_THIN: u32 = 13;

/// A 2D texture (T#, 8 dwords; radv's sampled-image descriptor is 64 bytes: this, then 32 bytes of zeros it reserves for
/// FMASK). `addr` 256-byte aligned; `pitch` in texels (linear aligned: a multiple of 64 for 32-bit texels).
pub fn texture_2d(addr: u64, width: u32, height: u32, pitch: u32, data_format: u32, num_format: u32, tiling_index: u32, levels: u32) -> [u32; 8] {
	assert!(addr % 256 == 0, "texture address must be 256-byte aligned");
	assert!(width >= 1 && height >= 1 && width <= 16384 && height <= 16384 && pitch >= width && levels >= 1);
	let a = addr >> 8;
	[
		a as u32,
		(a >> 32) as u32 & 0x3f | data_format << 20 | num_format << 26,
		(width - 1) | (height - 1) << 14,
		SEL_X | SEL_Y << 3 | SEL_Z << 6 | SEL_W << 9 | (levels - 1) << 16 | tiling_index << 20 | IMG_TYPE_2D << 28,
		(pitch - 1) << 13,
		0,
		0,
		0,
	]
}

/// Sampler wrap modes (SQ_TEX_WRAP / CLAMP_LAST_TEXEL / MIRROR) and filters.
pub const WRAP_REPEAT: u32 = 0;
pub const WRAP_MIRROR: u32 = 1;
pub const WRAP_CLAMP: u32 = 2;
pub const FILTER_POINT: u32 = 0;
pub const FILTER_BILINEAR: u32 = 1;

/// A sampler (S#): `wrap` for U and V, `filter` for magnification and minification, mips off (LOD range 0..15).
pub fn sampler(wrap: u32, filter: u32) -> [u32; 4] {
	[
		wrap | wrap << 3 | wrap << 6,
		15 << 8 << 12, // MAX_LOD = 15.0 (4.8 fixed point), MIN_LOD = 0
		filter << 20 | filter << 22,
		0,
	]
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn buffer_fields() {
		let d = buffer(0x1_2345_6780, 12, 36);
		assert_eq!(d[0], 0x2345_6780);
		assert_eq!(d[1] & 0xffff, 1);
		assert_eq!(d[1] >> 16, 12);
		assert_eq!(d[2], 36);
		// Type (bits 30-31 of word 3) must be 0 for a buffer.
		assert_eq!(d[3] >> 30, 0);
	}

	#[test]
	fn texture_fields_match_shadps4_layout() {
		// shadPS4 resource.h Image: base_address:38, mtype_l2:2, min_lod:12, data_format:6, num_format:4, mtype:2 (qword 0);
		// width:14 height:14 perf:3 interlaced:1 dst_sel x/y/z/w:3 each, base_level:4 last_level:4 tiling_index:5 pow2pad:1
		// mtype2:1 atc:1 type:4 (qword 1); depth:13 pitch:14 (qword 2).
		let d = texture_2d(0x2_0000_0100, 256, 128, 256, IMG_DATA_FORMAT_8_8_8_8, IMG_NUM_FORMAT_UNORM, 8, 1);
		let q0 = d[0] as u64 | (d[1] as u64) << 32;
		let q1 = d[2] as u64 | (d[3] as u64) << 32;
		let q2 = d[4] as u64 | (d[5] as u64) << 32;
		assert_eq!(q0 & ((1 << 38) - 1), 0x2_0000_0100 >> 8);
		assert_eq!((q0 >> 52) & 0x3f, 10);
		assert_eq!((q0 >> 58) & 0xf, 0);
		assert_eq!(q1 & 0x3fff, 255);
		assert_eq!((q1 >> 14) & 0x3fff, 127);
		assert_eq!((q1 >> 32) & 7, SEL_X as u64);
		assert_eq!((q1 >> 41) & 7, SEL_W as u64);
		assert_eq!((q1 >> 52) & 0x1f, 8);
		assert_eq!((q1 >> 60) & 0xf, 9);
		assert_eq!((q2 >> 13) & 0x3fff, 255);
	}
}
