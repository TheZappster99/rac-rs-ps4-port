//! Shader-cache entries (.rsh, version 1; written by ps4/tools/radvbin from Mesa radv 26.1.6 GFX7 binaries, format in its
//! main.rs) and the register values the PS4 driver's shader calls take. radv recomputes these registers when it loads a
//! pipeline binary (radv_precompute_registers_*); the same rules are here for the GFX7 hardware VS and PS.

use crate::pm4::Cmd;
use crate::regs;

/// Stage of a cache entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
	Vertex,
	Fragment,
	Compute,
}

/// User-data slots (radv's AC_UD_* indices, Mesa 26.1.6).
pub mod ud {
	pub const SCRATCH_RING_OFFSETS: usize = 0;
	pub const PUSH_CONSTANTS: usize = 1;
	pub const INLINE_PUSH_CONSTANTS: usize = 2;
	pub const INDIRECT_DESCRIPTORS: usize = 3;
	pub const VS_VERTEX_BUFFERS: usize = 20;
	pub const VS_BASE_VERTEX_START_INSTANCE: usize = 21;
	pub const VS_PROLOG_INPUTS: usize = 22;
	pub const CS_GRID_SIZE: usize = 20;
	pub const MAX: usize = 28;
}

/// A user SGPR location: first SGPR and how many.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loc {
	pub sgpr: u32,
	pub count: u32,
}

/// A parsed cache entry. `code` borrows the entry's bytes.
pub struct Shader<'a> {
	pub stage: Stage,
	pub num_sgprs: u32,
	pub num_vgprs: u32,
	pub scratch_bytes_per_wave: u32,
	pub rsrc1: u32,
	pub rsrc2: u32,
	pub rsrc3: u32,
	pub desc_set_used_mask: u32,
	pub push_constant_size: u32,
	pub inline_push_constant_mask: u64,
	pub ps_input_ena: u32,
	pub ps_input_addr: u32,
	pub ps_col_format: u32,
	pub ps_cb_shader_mask: u32,
	pub ps_num_inputs: u32,
	pub ps_input_mask: u32,
	/// radv's `explicit_shaded_mask` (inputs read per vertex; historically named "flat" here).
	pub ps_flat_mask: u32,
	/// radv's interpolated inputs (32-bit and 16-bit), when the cache carries them (`ps_masks`); the other inputs are flat.
	pub ps_float32_mask: u32,
	pub ps_float16_mask: u32,
	/// Whether the two masks above came with the shader (caches packed before 2026-11-10 lack them).
	pub ps_masks: bool,
	pub vs_param_exports: u32,
	pub vs_clip_cull_pointsize: u32,
	pub vs_vb_desc_usage_mask: u32,
	pub cs_block: [u32; 3],
	sets: [Option<Loc>; 32],
	data: [Option<Loc>; ud::MAX],
	/// VS output parameter index per varying slot (0xff = not written).
	pub vs_param_offset: [u8; 64],
	pub code: &'a [u8],
}

const MAGIC: u32 = u32::from_le_bytes(*b"RSH1");
const HEADER_WORDS: usize = 32;
const UD_BYTES: usize = 64 + 2 * ud::MAX + 64;

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
	TooShort,
	BadMagic,
	BadStage(u32),
	CodeSize,
}

impl<'a> Shader<'a> {
	pub fn parse(b: &'a [u8]) -> Result<Shader<'a>, ParseError> {
		if b.len() < HEADER_WORDS * 4 + UD_BYTES {
			return Err(ParseError::TooShort);
		}
		let w = |i: usize| u32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap());
		if w(0) != MAGIC {
			return Err(ParseError::BadMagic);
		}
		let stage = match w(1) {
			0 => Stage::Vertex,
			1 => Stage::Fragment,
			2 => Stage::Compute,
			s => return Err(ParseError::BadStage(s)),
		};
		let u = &b[HEADER_WORDS * 4..HEADER_WORDS * 4 + UD_BYTES];
		let loc = |i: usize| {
			let (s, n) = (u[2 * i] as i8, u[2 * i + 1]);
			(s >= 0).then_some(Loc { sgpr: s as u32, count: n as u32 })
		};
		let mut sets = [None; 32];
		for (i, s) in sets.iter_mut().enumerate() {
			*s = loc(i);
		}
		let mut data = [None; ud::MAX];
		for (i, d) in data.iter_mut().enumerate() {
			*d = loc(32 + i);
		}
		let mut vs_param_offset = [0u8; 64];
		vs_param_offset.copy_from_slice(&u[64 + 2 * ud::MAX..]);
		let code_len = w(30) as usize;
		let code_at = HEADER_WORDS * 4 + UD_BYTES;
		if code_at + code_len != b.len() || code_len == 0 || code_len % 4 != 0 {
			return Err(ParseError::CodeSize);
		}
		Ok(Shader {
			stage,
			num_sgprs: w(2),
			num_vgprs: w(3),
			scratch_bytes_per_wave: w(4),
			rsrc1: w(7),
			rsrc2: w(8),
			rsrc3: w(9),
			desc_set_used_mask: w(11),
			push_constant_size: w(12),
			inline_push_constant_mask: w(13) as u64 | (w(14) as u64) << 32,
			ps_input_ena: w(15),
			ps_input_addr: w(16),
			ps_col_format: w(17),
			ps_cb_shader_mask: w(18),
			ps_num_inputs: w(19),
			ps_input_mask: w(20),
			ps_flat_mask: w(21),
			ps_float32_mask: if stage == Stage::Fragment { w(23) } else { 0 },
			ps_float16_mask: if stage == Stage::Fragment { w(24) & 0x7fff_ffff } else { 0 },
			ps_masks: stage == Stage::Fragment && w(24) >> 31 != 0,
			vs_param_exports: w(23),
			vs_clip_cull_pointsize: w(24),
			vs_vb_desc_usage_mask: w(25),
			cs_block: [w(27), w(28), w(29)],
			sets,
			data,
			vs_param_offset,
			code: &b[code_at..],
		})
	}

	/// The user SGPR holding descriptor set `set`'s 32-bit address, if the shader uses it.
	pub fn set_sgpr(&self, set: usize) -> Option<Loc> {
		self.sets.get(set).copied().flatten()
	}

	/// A user-data slot (`ud::*`).
	pub fn ud(&self, slot: usize) -> Option<Loc> {
		self.data.get(slot).copied().flatten()
	}

	/// User SGPRs the hardware loads (RSRC2.USER_SGPR).
	pub fn user_sgprs(&self) -> u32 {
		(self.rsrc2 >> 1) & 0x1f
	}

	/// sceGnmSetVsShader's structure: pgm lo/hi, rsrc1, rsrc2, SPI_VS_OUT_CONFIG, SPI_SHADER_POS_FORMAT, PA_CL_VS_OUT_CNTL.
	/// `addr` = the code's GPU address (256-byte aligned).
	pub fn vs_regs(&self, addr: u64) -> [u32; 7] {
		assert_eq!(self.stage, Stage::Vertex);
		assert!(addr % 256 == 0);
		let clip = self.vs_clip_cull_pointsize & 0xff;
		let cull = (self.vs_clip_cull_pointsize >> 8) & 0xff;
		let psize = (self.vs_clip_cull_pointsize >> 16) & 1;
		// VS_EXPORT_COUNT = parameters - 1 (at least one is always exported).
		let out_config = self.vs_param_exports.max(1) - 1;
		// POS0 4 components; POS1 when point size or clip / cull distances are written (radv's misc vector).
		let misc = psize != 0 || clip | cull != 0;
		let pos_format = 4 | if misc { 4 << 4 } else { 0 };
		let mut out_cntl = 0u32;
		if misc {
			// USE_VTX_POINT_SIZE (16), VS_OUT_MISC_VEC_ENA (24), VS_OUT_CCDIST0_VEC_ENA (25), CLIP / CULL_DIST enables (0-15).
			out_cntl |= psize << 16 | 1 << 24 | ((clip | cull != 0) as u32) << 25 | clip | cull << 8;
		}
		[(addr >> 8) as u32, (addr >> 40) as u32, self.rsrc1, self.rsrc2, out_config << 1, pos_format, out_cntl]
	}

	/// sceGnmSetPsShader350's structure: pgm lo/hi, rsrc1, rsrc2, SPI_SHADER_Z_FORMAT, SPI_SHADER_COL_FORMAT, SPI_PS_INPUT_ENA,
	/// SPI_PS_INPUT_ADDR, SPI_PS_IN_CONTROL, SPI_BARYC_CNTL, DB_SHADER_CONTROL, CB_SHADER_MASK.
	pub fn ps_regs(&self, addr: u64) -> [u32; 12] {
		assert_eq!(self.stage, Stage::Fragment);
		assert!(addr % 256 == 0);
		// Z_ORDER early-then-late when the shader cannot discard is radv's choice too; this layer does not know whether a shader
		// kills pixels, so it takes the always-correct late Z (1 << 4 would be early-then-late).
		let db_shader_control = 0;
		[
			(addr >> 8) as u32,
			(addr >> 40) as u32,
			self.rsrc1,
			self.rsrc2,
			0, // no depth export (SPI_SHADER_ZERO)
			self.ps_col_format,
			self.ps_input_ena,
			self.ps_input_addr,
			self.ps_num_inputs.min(32), // SPI_PS_IN_CONTROL: NUM_INTERP
			0,
			db_shader_control,
			self.ps_cb_shader_mask,
		]
	}

	/// SPI_PS_INPUT_CNTL_n for this PS fed by `vs`: input n (n-th set bit of the PS input mask, a varying VAR k) reads the VS
	/// parameter that VAR k was exported to (OFFSET, bits 0-5); inputs the VS does not write read the default (OFFSET 0x20, 0).
	/// As radv (radv_shader.c `radv_ps_input_type_bits`, radv_cmd_buffer.c `radv_emit_ps_inputs`): an explicit (per-vertex)
	/// input gets OFFSET bit 5 and FLAT_SHADE (bit 10), an interpolated one nothing more, and every other input (WGSL
	/// `@interpolate(flat)`, a plain load) FLAT_SHADE. `faithful` = false, or a cache without the interpolation masks, keeps the
	/// old words (explicit inputs bit 5 only, no FLAT_SHADE anywhere).
	pub fn ps_input_cntl_with(&self, vs: &Shader, faithful: bool) -> Vec<u32> {
		assert_eq!(self.stage, Stage::Fragment);
		const FLAT_SHADE: u32 = 1 << 10;
		let faithful = faithful && self.ps_masks;
		let mut v = Vec::new();
		let mut m = self.ps_input_mask;
		while m != 0 {
			let k = m.trailing_zeros() as usize;
			m &= m - 1;
			let off = vs.vs_param_offset[32 + k];
			if off == 0xff {
				v.push(0x20);
				continue;
			}
			let mut cntl = off as u32 & 0x1f;
			let bit = 1u32 << k;
			if self.ps_flat_mask & bit != 0 {
				cntl |= 1 << 5 | if faithful { FLAT_SHADE } else { 0 };
			} else if faithful && (self.ps_float32_mask | self.ps_float16_mask) & bit == 0 {
				cntl |= FLAT_SHADE;
			}
			v.push(cntl);
		}
		v
	}

	/// `ps_input_cntl_with(vs, true)`: radv's words when the cache carries the masks.
	pub fn ps_input_cntl(&self, vs: &Shader) -> Vec<u32> {
		self.ps_input_cntl_with(vs, true)
	}
}

/// Writes the VS and PS of a graphics pipeline whose code lives at `vs_addr` / `ps_addr`: the driver's helpers fill reserved
/// space (`set_vs` / `set_ps` callbacks, console only), then the CU masks, the PS input mapping and the shader stages enable.
/// Returns the two helpers' results.
pub fn bind_graphics(c: &mut Cmd, vs: &Shader, ps: &Shader, set_vs: impl FnOnce(&mut [u32], &[u32; 7]) -> i32, set_ps: impl FnOnce(&mut [u32], &[u32; 12]) -> i32, vs_addr: u64, ps_addr: u64) -> (i32, i32) {
	let rv = set_vs(c.reserve(29), &vs.vs_regs(vs_addr));
	let rp = set_ps(c.reserve(40), &ps.ps_regs(ps_addr));
	// All compute units for both stages, no wave limit (as in the console-proven triangle test).
	c.sh(regs::SPI_SHADER_PGM_RSRC3_PS, &[0xffff]);
	c.sh(regs::SPI_SHADER_PGM_RSRC3_VS, &[0xffff, 0]); // + SPI_SHADER_LATE_ALLOC_VS
	let cntl = ps.ps_input_cntl(vs);
	if !cntl.is_empty() {
		c.ctx(regs::SPI_PS_INPUT_CNTL_0, &cntl);
	}
	c.ctx(regs::VGT_SHADER_STAGES_EN, &[0]);
	(rv, rp)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn entry(stage: u32, f: impl FnOnce(&mut [u32; 32], &mut [u8; 184])) -> Vec<u8> {
		let mut w = [0u32; 32];
		w[0] = MAGIC;
		w[1] = stage;
		w[30] = 8;
		let mut u = [0xffu8; 184];
		for i in 0..60 {
			u[2 * i + 1] = 0;
		}
		f(&mut w, &mut u);
		let mut b: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
		b.extend_from_slice(&u);
		b.extend_from_slice(&[0, 0, 0x81, 0xbf, 0, 0, 0x81, 0xbf]);
		b
	}

	#[test]
	fn parse_and_registers() {
		// The cube_vs of ps4/gnmtest as radvbin printed it: set 0 at SGPR 4, vertex buffers at 2, base vertex at 3.
		let vs = entry(0, |w, u| {
			w[7] = 0x002c_0082;
			w[8] = 0xa;
			w[23] = 1;
			u[0] = 4;
			u[1] = 1;
			u[64] = 0;
			u[65] = 2;
			u[64 + 2 * 20] = 2;
			u[64 + 2 * 20 + 1] = 1;
			u[64 + 2 * 21] = 3;
			u[64 + 2 * 21 + 1] = 1;
			u[64 + 56 + 32] = 0; // VAR0 -> param 0
		});
		let vs = Shader::parse(&vs).unwrap();
		assert_eq!(vs.stage, Stage::Vertex);
		assert_eq!(vs.set_sgpr(0), Some(Loc { sgpr: 4, count: 1 }));
		assert_eq!(vs.set_sgpr(1), None);
		assert_eq!(vs.ud(ud::VS_VERTEX_BUFFERS), Some(Loc { sgpr: 2, count: 1 }));
		assert_eq!(vs.user_sgprs(), 5);
		let r = vs.vs_regs(0x1_0000_0100);
		assert_eq!(r[0], 0x0100_0001);
		assert_eq!(r[4], 0);
		assert_eq!(r[5], 4);
		let ps = entry(1, |w, _| {
			w[15] = 2;
			w[16] = 2;
			w[17] = 4;
			w[18] = 0xf;
			w[19] = 1;
			w[20] = 1;
		});
		let ps = Shader::parse(&ps).unwrap();
		assert_eq!(ps.ps_input_cntl(&vs), vec![0]);
		assert_eq!(ps.ps_regs(0x200)[8], 1);
	}

	/// radv's SPI_PS_INPUT_CNTL words: interpolated inputs plain, flat ones FLAT_SHADE, explicit ones OFFSET bit 5 + FLAT_SHADE,
	/// unexported ones the default; without the masks (an old cache) the old words.
	#[test]
	fn ps_input_words_follow_radv() {
		let vb = entry(0, |w, _| w[23] = 3);
		let mut vs = Shader::parse(&vb).unwrap();
		vs.vs_param_offset[32] = 0;
		vs.vs_param_offset[33] = 1;
		vs.vs_param_offset[34] = 2;
		vs.vs_param_offset[35] = 0xff;
		let bytes = |masks: bool| {
			entry(1, |w, _| {
				w[15] = 2;
				w[16] = 2;
				w[19] = 4;
				w[20] = 0b1111;
				w[21] = 0b0100; // VAR2 explicit
				w[23] = 0b0001; // VAR0 interpolated; VAR1 flat
				w[24] = if masks { 1 << 31 } else { 0 };
			})
		};
		let (with, without) = (bytes(true), bytes(false));
		let (with, without) = (Shader::parse(&with).unwrap(), Shader::parse(&without).unwrap());
		assert_eq!(with.ps_input_cntl(&vs), vec![0, 1 | 1 << 10, 2 | 1 << 5 | 1 << 10, 0x20]);
		assert_eq!(with.ps_input_cntl_with(&vs, false), vec![0, 1, 2 | 1 << 5, 0x20]);
		assert_eq!(without.ps_input_cntl(&vs), vec![0, 1, 2 | 1 << 5, 0x20]);
	}

	#[test]
	fn rejects_bad_entries() {
		assert_eq!(Shader::parse(&[0; 10]).err(), Some(ParseError::TooShort));
		let mut b = entry(0, |_, _| {});
		b[0] = b'X';
		assert_eq!(Shader::parse(&b).err(), Some(ParseError::BadMagic));
		let mut b = entry(0, |_, _| {});
		b.push(0);
		assert_eq!(Shader::parse(&b).err(), Some(ParseError::CodeSize));
	}
}
