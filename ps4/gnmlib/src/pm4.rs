//! PM4 type-3 command buffers. A `Cmd` writes into any u32 slice (GPU-visible memory on the console, a Vec in tests); the
//! driver's own helpers (sceGnmSetVsShader, ...) write into `reserve`d space.

use crate::regs::{self, op};

pub struct Cmd<'a> {
	buf: &'a mut [u32],
	n: usize,
}

/// The header of a type-3 packet with `body` dwords after it.
pub const fn header(opcode: u32, body: usize) -> u32 {
	(3 << 30) | (((body as u32) - 1) << 16) | (opcode << 8)
}

impl<'a> Cmd<'a> {
	pub fn new(buf: &'a mut [u32]) -> Cmd<'a> {
		Cmd { buf, n: 0 }
	}

	/// Dwords written so far.
	pub fn len(&self) -> usize {
		self.n
	}

	pub fn is_empty(&self) -> bool {
		self.n == 0
	}

	pub fn words(&self) -> &[u32] {
		&self.buf[..self.n]
	}

	fn push(&mut self, v: u32) {
		assert!(self.n < self.buf.len(), "command buffer full ({} dwords)", self.buf.len());
		self.buf[self.n] = v;
		self.n += 1;
	}

	/// A type-3 packet.
	pub fn pkt3(&mut self, opcode: u32, body: &[u32]) {
		assert!(!body.is_empty() && body.len() <= 0x3fff);
		self.push(header(opcode, body.len()));
		for &v in body {
			self.push(v);
		}
	}

	fn regs(&mut self, opcode: u32, base: u32, addr: u32, vals: &[u32]) {
		assert!(addr >= base && addr % 4 == 0, "register {addr:#x} not in the {base:#x} block");
		assert!(!vals.is_empty());
		self.push(header(opcode, vals.len() + 1));
		self.push((addr - base) / 4);
		for &v in vals {
			self.push(v);
		}
	}

	/// SET_CONTEXT_REG from `addr` on (consecutive registers).
	pub fn ctx(&mut self, addr: u32, vals: &[u32]) {
		self.regs(op::SET_CONTEXT_REG, 0x28000, addr, vals);
	}

	/// SET_SH_REG from `addr` on.
	pub fn sh(&mut self, addr: u32, vals: &[u32]) {
		self.regs(op::SET_SH_REG, 0xb000, addr, vals);
	}

	/// SET_UCONFIG_REG from `addr` on.
	pub fn uconfig(&mut self, addr: u32, vals: &[u32]) {
		self.regs(op::SET_UCONFIG_REG, 0x30000, addr, vals);
	}

	/// `n` dwords for a driver helper that writes its packets itself; returns where they start.
	pub fn reserve(&mut self, n: usize) -> &mut [u32] {
		assert!(self.n + n <= self.buf.len(), "command buffer full ({} dwords)", self.buf.len());
		let s = self.n;
		self.n += n;
		&mut self.buf[s..s + n]
	}

	/// A user SGPR block of the VS (`first` = user SGPR index).
	pub fn user_vs(&mut self, first: u32, vals: &[u32]) {
		self.sh(regs::SPI_SHADER_USER_DATA_VS_0 + 4 * first, vals);
	}

	/// A user SGPR block of the PS.
	pub fn user_ps(&mut self, first: u32, vals: &[u32]) {
		self.sh(regs::SPI_SHADER_USER_DATA_PS_0 + 4 * first, vals);
	}

	/// End of pipe: flush and invalidate the colour and depth caches, then write the 32-bit `value` to `addr` (4-byte
	/// aligned, GPU address). The CPU waits for the label to know a frame is done.
	pub fn eop_label(&mut self, addr: u64, value: u32) {
		// EVENT_TYPE CACHE_FLUSH_AND_INV_TS_EVENT (0x14), EVENT_INDEX 5; DATA_SEL 1 = write the low 32 bits.
		self.pkt3(op::EVENT_WRITE_EOP, &[0x14 | (5 << 8), addr as u32, ((addr >> 32) as u32 & 0xffff) | (1 << 29), value, 0]);
	}

	/// Invalidates the shader caches before work that reads memory the CPU (or earlier GPU work) changed: the scalar cache
	/// (K$, uniform reads), the instruction cache, the vector L1 (TCL1) and the L2 (TC). ACQUIRE_MEM over the whole address
	/// range, as Mesa does on GFX7 (CP_COHER_CNTL bits: TC 23, TCL1 22, SH_KCACHE 27, SH_ICACHE 29).
	pub fn invalidate_shader_caches(&mut self) {
		let cntl = 1 << 23 | 1 << 22 | 1 << 27 | 1 << 29;
		self.pkt3(op::ACQUIRE_MEM, &[cntl, 0xffff_ffff, 0x00ff_ffff, 0, 0, 0x0000_000a]);
	}

	/// NUM_INSTANCES.
	pub fn num_instances(&mut self, n: u32) {
		self.pkt3(op::NUM_INSTANCES, &[n]);
	}

	/// VGT_PRIMITIVE_TYPE.
	pub fn primitive(&mut self, prim: u32) {
		self.uconfig(regs::VGT_PRIMITIVE_TYPE, &[prim]);
	}

	/// Pads with a NOP packet to a multiple of `n` dwords (n >= 2).
	pub fn pad_to(&mut self, n: usize) {
		let rem = self.n % n;
		if rem != 0 {
			let fill = n - rem;
			if fill == 1 {
				// A 1-dword filler: a type-2 packet (GFX7 still accepts them).
				self.push(0x8000_0000);
			} else {
				self.push(header(op::NOP, fill - 1));
				for _ in 0..fill - 1 {
					self.push(0);
				}
			}
		}
	}
}

/// The most dwords one submitted command buffer may hold. The CP fetches each one through an INDIRECT_BUFFER packet whose
/// size field is 20 bits of dwords, so a buffer of 4 MB or more would be cut short and the GPU would run into whatever
/// follows (a hang). A level frame of the real game is bigger than that, so a submission is split into several buffers,
/// kept well under the limit.
pub const MAX_SUBMIT_DWORDS: usize = 0xF_0000;

/// Splits a stream of PM4 packets into pieces of at most `max` dwords, each ending on a packet boundary. Returns the
/// pieces' (start, length), or `Err(at)` with the dword index of a header it cannot size (type 1, or a packet running past
/// the end), in which case the caller must not split.
pub fn split(words: &[u32], max: usize) -> Result<Vec<(usize, usize)>, usize> {
	let mut out = Vec::new();
	let (mut start, mut at) = (0usize, 0usize);
	while at < words.len() {
		let h = words[at];
		let len = match h >> 30 {
			// Type 0 (register writes) and type 3: COUNT + 2 dwords.
			0 | 3 => ((h >> 16) & 0x3fff) as usize + 2,
			// Type 2: a one-dword filler.
			2 => 1,
			_ => return Err(at),
		};
		if at + len > words.len() || len > max {
			return Err(at);
		}
		if at + len - start > max {
			out.push((start, at - start));
			start = at;
		}
		at += len;
	}
	if at > start {
		out.push((start, at - start));
	}
	Ok(out)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn split_on_packet_boundaries() {
		let mut b = vec![0u32; 64];
		let mut c = Cmd::new(&mut b);
		c.pkt3(0x10, &[1, 2, 3]); // 4 dwords
		c.pkt3(0x10, &[1]); // 2
		c.pkt3(0x10, &[1, 2, 3, 4, 5]); // 6
		let n = c.len();
		let mut w = b[..n].to_vec();
		w.push(0x8000_0000); // type 2 filler
		assert_eq!(split(&w, 100), Ok(vec![(0, 13)]));
		let p = split(&w, 6).unwrap();
		assert_eq!(p, vec![(0, 6), (6, 6), (12, 1)]);
		assert!(p.iter().all(|&(_, l)| l <= 6));
		// A packet bigger than the limit, or running past the end, cannot be split.
		assert_eq!(split(&w, 5), Err(6));
		assert_eq!(split(&w[..5], 100), Err(4));
		assert_eq!(split(&[0x4000_0000], 100), Err(0));
	}

	#[test]
	fn packets_match_the_triangle_test() {
		let mut b = [0u32; 32];
		let mut c = Cmd::new(&mut b);
		// The ps4/gnm triangle test's CB_TARGET_MASK write, which ran on the console.
		c.ctx(0x28238, &[0xf]);
		assert_eq!(c.words(), &[0xc001_6900, 0x8e, 0xf]);
		let mut b = [0u32; 32];
		let mut c = Cmd::new(&mut b);
		c.sh(0xb130, &[7]);
		assert_eq!(c.words(), &[0xc001_7600, 0x4c, 7]);
		let mut b = [0u32; 32];
		let mut c = Cmd::new(&mut b);
		c.uconfig(0x30908, &[4]);
		assert_eq!(c.words(), &[0xc001_7900, 0x242, 4]);
	}

	#[test]
	fn acquire_mem_is_seven_dwords() {
		let mut b = [0u32; 16];
		let mut c = Cmd::new(&mut b);
		c.invalidate_shader_caches();
		// Mesa: PKT3(PKT3_ACQUIRE_MEM, 5, 0) then 6 dwords.
		assert_eq!(c.words()[0], 0xc005_5800);
		assert_eq!(c.len(), 7);
	}

	#[test]
	fn padding() {
		let mut b = [0u32; 32];
		let mut c = Cmd::new(&mut b);
		c.num_instances(1);
		c.pad_to(8);
		assert_eq!(c.len(), 8);
	}
}
