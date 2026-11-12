//! PS4 port, step 2 part 3: one triangle drawn by the PS4's GPU through GNM, the console's native graphics driver.
//!
//! Piglet (the system's OpenGL ES) would not start and the console has no shader compiler, so the renderer has to talk to the
//! GPU the way PS4 games do: shaders are GCN machine code (here compiled on the PC by the private toolchain's LLVM, see
//! ps4/gnm/shaders) and drawing is a PM4 command buffer we write ourselves. The driver's own helpers write the parts whose
//! layout only it knows (default hardware state, shader registers, the draw packet); the rest are plain register writes.
//!
//! Each frame: build a command buffer (render target = one of the video out buffers, viewport, shaders, 6 vertices: a
//! background triangle and an RGB triangle the CPU turns a little every frame), submit it, wait for the end-of-pipe label the
//! GPU writes when it is done, then flip that buffer with sceVideoOut from the CPU. Everything goes to /data/rac-rs/gnm.log,
//! including the colour of a few pixels after the first frame (what the GPU wrote, even if the flip shows nothing).

use std::io::Write;
use std::time::{Duration, Instant};

use rac_ps4_shim as _;

const LOG: &str = "/data/rac-rs/gnm.log";

struct Log(Option<std::fs::File>);

impl Log {
	fn say(&mut self, s: impl AsRef<str>) {
		if let Some(f) = self.0.as_mut() {
			let _ = writeln!(f, "{}", s.as_ref());
			let _ = f.flush();
		}
	}
}

fn main() {
	#[cfg(target_os = "freebsd")]
	rac_ps4_shim::init();
	let _ = std::fs::create_dir_all("/data/rac-rs");
	let mut log = Log(std::fs::File::create(LOG).ok());
	let lp = LOG.to_string();
	std::panic::set_hook(Box::new(move |p| {
		if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&lp) {
			let _ = writeln!(f, "gnm: PANIC {p}");
		}
	}));
	#[cfg(target_os = "freebsd")]
	{
		// SAFETY: the whole test is FFI into the PS4's libraries with memory it maps itself.
		unsafe { ps4::run(&mut log) };
		log.say("gnm: done");
		rac_ps4_shim::quit(0);
	}
	#[cfg(not(target_os = "freebsd"))]
	log.say("gnm: PS4 only");
}

/// One shader as ps4/build.sh compiled it: machine code and the register values llc reported (`.AMDGPU.config`: pairs of
/// register byte address and value).
struct Shader {
	code: &'static [u8],
	cfg: &'static str,
}

impl Shader {
	fn reg(&self, addr: u32) -> u32 {
		self.cfg
			.lines()
			.filter_map(|l| {
				let mut it = l.split_whitespace();
				Some((it.next()?.parse::<u32>().ok()?, it.next()?.parse::<u32>().ok()?))
			})
			.find(|&(a, _)| a == addr)
			.map_or(0, |(_, v)| v)
	}
}

static VS: Shader = Shader { code: include_bytes!(concat!(env!("RAC_GNM_SHADERS"), "/tri_vs.bin")), cfg: include_str!(concat!(env!("RAC_GNM_SHADERS"), "/tri_vs.cfg")) };
static PS: Shader = Shader { code: include_bytes!(concat!(env!("RAC_GNM_SHADERS"), "/tri_ps.bin")), cfg: include_str!(concat!(env!("RAC_GNM_SHADERS"), "/tri_ps.cfg")) };

/// The screen, and the video out buffers the GPU draws into (linear, A8R8G8B8).
const W: u32 = 1920;
const H: u32 = 1080;

// Register byte addresses (AMD Sea Islands / CIK, as in Mesa's sid.h). Context registers live at 0x28000+, persistent
// shader (SH) registers at 0xB000+, user-config registers at 0x30000+.
const SPI_SHADER_PGM_RSRC1_VS: u32 = 0xb128;
const SPI_SHADER_PGM_RSRC1_PS: u32 = 0xb028;
const SPI_SHADER_PGM_RSRC2_PS: u32 = 0xb02c;
const SPI_PS_INPUT_ENA: u32 = 0x286cc;
const SPI_PS_INPUT_ADDR: u32 = 0x286d0;

/// A PM4 command buffer being written (type-3 packets).
struct Cmd {
	p: *mut u32,
	n: usize,
	cap: usize,
}

impl Cmd {
	fn push(&mut self, v: u32) {
		assert!(self.n < self.cap, "command buffer full");
		// SAFETY: inside the mapped command buffer (checked above).
		unsafe { self.p.add(self.n).write_volatile(v) };
		self.n += 1;
	}
	/// A type-3 packet: header (count = body dwords - 1, opcode) then the body.
	fn pkt3(&mut self, op: u32, body: &[u32]) {
		self.push((3 << 30) | (((body.len() as u32) - 1) << 16) | (op << 8));
		for &v in body {
			self.push(v);
		}
	}
	fn regs(&mut self, op: u32, base: u32, addr: u32, vals: &[u32]) {
		let mut b = vec![(addr - base) / 4];
		b.extend_from_slice(vals);
		self.pkt3(op, &b);
	}
	/// SET_CONTEXT_REG.
	fn ctx(&mut self, addr: u32, vals: &[u32]) {
		self.regs(0x69, 0x28000, addr, vals);
	}
	/// SET_SH_REG.
	fn sh(&mut self, addr: u32, vals: &[u32]) {
		self.regs(0x76, 0xb000, addr, vals);
	}
	/// SET_UCONFIG_REG.
	fn uconfig(&mut self, addr: u32, vals: &[u32]) {
		self.regs(0x79, 0x30000, addr, vals);
	}
	/// `n` dwords for a driver helper that writes its packets itself.
	fn reserve(&mut self, n: usize) -> *mut u32 {
		assert!(self.n + n <= self.cap, "command buffer full");
		// SAFETY: inside the mapped command buffer (checked above).
		let p = unsafe { self.p.add(self.n) };
		self.n += n;
		p
	}
}

#[cfg(target_os = "freebsd")]
mod ps4 {
	use super::*;
	use core::ffi::{c_char, c_void};

	#[repr(C)]
	#[derive(Default)]
	struct BufferAttribute {
		format: i32,
		tmode: i32,
		aspect: i32,
		width: u32,
		height: u32,
		pitch: u32,
		reserved: [u64; 2],
	}

	unsafe extern "C" {
		fn sceVideoOutOpen(user: i32, bus: i32, index: i32, param: *const c_void) -> i32;
		fn sceVideoOutSetBufferAttribute(attr: *mut BufferAttribute, format: u32, tmode: u32, aspect: u32, w: u32, h: u32, pitch: u32);
		fn sceVideoOutRegisterBuffers(h: i32, start: i32, addrs: *const *mut c_void, n: i32, attr: *const BufferAttribute) -> i32;
		fn sceVideoOutSubmitFlip(h: i32, idx: i32, mode: u32, arg: i64) -> i32;
		fn sceVideoOutSetFlipRate(h: i32, rate: i32) -> i32;
		fn sceVideoOutAddFlipEvent(eq: u64, h: i32, udata: *mut c_void) -> i32;
		fn sceKernelCreateEqueue(eq: *mut u64, name: *const c_char) -> i32;
		fn sceKernelWaitEqueue(eq: u64, ev: *mut [u64; 4], n: i32, out: *mut i32, timeout: *mut u32) -> i32;
		fn sceKernelGetDirectMemorySize() -> usize;
		fn sceKernelAllocateDirectMemory(start: i64, end: i64, len: usize, align: usize, mem_type: i32, phys: *mut i64) -> i32;
		fn sceKernelMapDirectMemory(addr: *mut *mut c_void, len: usize, prot: i32, flags: i32, phys: i64, align: usize) -> i32;

		fn sceGnmDrawInitDefaultHardwareState350(cmd: *mut u32, n: u32) -> i32;
		fn sceGnmSetVsShader(cmd: *mut u32, n: u32, regs: *const u32, modifier: u32) -> i32;
		fn sceGnmSetPsShader350(cmd: *mut u32, n: u32, regs: *const u32) -> i32;
		fn sceGnmDrawIndexAuto(cmd: *mut u32, n: u32, count: u32, flags: u32) -> i32;
		fn sceGnmSubmitCommandBuffers(count: u32, dcb: *const *mut c_void, dcb_bytes: *const u32, ccb: *const *mut c_void, ccb_bytes: *const u32) -> i32;
		fn sceGnmSubmitDone() -> i32;
		fn sceGnmFlushGarlic();
	}

	/// Layout of the one direct-memory block: three render targets (also the video out buffers), two command buffers, the
	/// shaders and the end-of-pipe label.
	const RT_BYTES: usize = (W * H * 4) as usize;
	const CMD_DW: usize = 16 << 10;
	const OFF_CMD: usize = RT_BYTES * 3;
	const OFF_SHADERS: usize = OFF_CMD + 2 * CMD_DW * 4;
	const OFF_LABEL: usize = OFF_SHADERS + (64 << 10);
	const TOTAL: usize = OFF_LABEL + 4096;

	pub unsafe fn run(log: &mut Log) {
		unsafe {
			log.say(format!("gnm: VS {} bytes, cfg [{}]; PS {} bytes, cfg [{}]", VS.code.len(), VS.cfg.replace('\n', "; "), PS.code.len(), PS.cfg.replace('\n', "; ")));

			// Memory: write-combined direct memory, CPU and GPU read/write, 2 MB aligned.
			let align = 2 << 20;
			let len = TOTAL.div_ceil(align) * align;
			let mut phys = 0i64;
			let r = sceKernelAllocateDirectMemory(0, sceKernelGetDirectMemorySize() as i64, len, align, 3, &mut phys);
			if r < 0 {
				log.say(format!("gnm: allocate direct memory failed {r:#x}"));
				return;
			}
			let mut base: *mut c_void = core::ptr::null_mut();
			let r = sceKernelMapDirectMemory(&mut base, len, 0x33, 0, phys, align);
			if r < 0 || base.is_null() {
				log.say(format!("gnm: map direct memory failed {r:#x}"));
				return;
			}
			let base = base as *mut u8;
			log.say(format!("gnm: {} MB of direct memory at {base:p}", len >> 20));
			let rt: [*mut u32; 3] = [0, 1, 2].map(|i| base.add(i * RT_BYTES) as *mut u32);
			// Magenta first: if the GPU writes nothing, the pixels stay magenta.
			for &p in &rt {
				core::slice::from_raw_parts_mut(p, (W * H) as usize).fill(0xffff00ff);
			}
			// The shaders, each 256-byte aligned (the program address registers hold address >> 8).
			let vs_addr = base.add(OFF_SHADERS);
			let ps_addr = base.add(OFF_SHADERS + 0x1000);
			core::ptr::copy_nonoverlapping(VS.code.as_ptr(), vs_addr, VS.code.len());
			core::ptr::copy_nonoverlapping(PS.code.as_ptr(), ps_addr, PS.code.len());
			let label = base.add(OFF_LABEL) as *mut u32;
			label.write_volatile(0);

			// Video out with the three render targets as its buffers (linear A8R8G8B8 sRGB).
			let h = sceVideoOutOpen(0xff, 0, 0, core::ptr::null());
			if h < 0 {
				log.say(format!("gnm: sceVideoOutOpen failed {h:#x}"));
				return;
			}
			let mut eq = 0u64;
			sceKernelCreateEqueue(&mut eq, c"rac-rs gnm flip".as_ptr());
			sceVideoOutAddFlipEvent(eq, h, core::ptr::null_mut());
			let mut attr = BufferAttribute::default();
			sceVideoOutSetBufferAttribute(&mut attr, 0x8000_0000, 1, 0, W, H, W);
			let addrs: Vec<*mut c_void> = rt.iter().map(|&p| p as *mut c_void).collect();
			let r = sceVideoOutRegisterBuffers(h, 0, addrs.as_ptr(), 3, &attr);
			log.say(format!("gnm: video out handle {h}, register buffers {r:#x}"));
			if r < 0 {
				return;
			}
			sceVideoOutSetFlipRate(h, 0);

			// Shader registers (the driver writes them: sceGnmSetVsShader / sceGnmSetPsShader350 take these structures).
			let vs_lo = (vs_addr as u64 >> 8) as u32;
			let vs_hi = (vs_addr as u64 >> 40) as u32;
			// RSRC2 USER_SGPR = 1: the angle in user data register 0 arrives in s0.
			let vs_regs: [u32; 7] = [vs_lo, vs_hi, VS.reg(SPI_SHADER_PGM_RSRC1_VS), 1 << 1, 0, 4, 0];
			let ps_lo = (ps_addr as u64 >> 8) as u32;
			let ps_hi = (ps_addr as u64 >> 40) as u32;
			let ps_regs: [u32; 12] = [
				ps_lo,
				ps_hi,
				PS.reg(SPI_SHADER_PGM_RSRC1_PS),
				PS.reg(SPI_SHADER_PGM_RSRC2_PS) & !(0x1f << 1),
				0,    // SPI_SHADER_Z_FORMAT: no depth export
				9,    // SPI_SHADER_COL_FORMAT: MRT0 as 32-bit floats (SPI_SHADER_32_ABGR)
				PS.reg(SPI_PS_INPUT_ENA),
				PS.reg(SPI_PS_INPUT_ADDR),
				1,    // SPI_PS_IN_CONTROL: NUM_INTERP 1 (the colour)
				0,    // SPI_BARYC_CNTL
				0x10, // DB_SHADER_CONTROL: Z_ORDER early-then-late
				0xf,  // CB_SHADER_MASK: MRT0 RGBA
			];
			log.say(format!("gnm: VS regs {vs_regs:x?}, PS regs {ps_regs:x?}"));

			let frames = 600u32;
			let t0 = Instant::now();
			let mut gpu_ms = 0f64;
			for frame in 0..frames {
				let idx = (frame % 3) as usize;
				let mut c = Cmd { p: base.add(OFF_CMD + (frame as usize % 2) * CMD_DW * 4) as *mut u32, n: 0, cap: CMD_DW };
				let r_init = sceGnmDrawInitDefaultHardwareState350(c.reserve(256), 256);
				let r_vs = sceGnmSetVsShader(c.reserve(29), 29, vs_regs.as_ptr(), 0);
				let r_ps = sceGnmSetPsShader350(c.reserve(40), 40, ps_regs.as_ptr());
				// All compute units for both stages (CU_EN 0xffff, no wave limit), no late VS allocation.
				c.sh(0xb01c, &[0xffff]); // SPI_SHADER_PGM_RSRC3_PS
				c.sh(0xb118, &[0xffff, 0]); // SPI_SHADER_PGM_RSRC3_VS, SPI_SHADER_LATE_ALLOC_VS
				// User data 0 of the VS: the triangle's angle.
				c.sh(0xb130, &[(frame as f32 * 0.03).to_bits()]); // SPI_SHADER_USER_DATA_VS_0
				// Colour target 0 = this frame's video out buffer: linear aligned (tile mode index 8), 8-8-8-8 unorm, B and R
				// swapped (memory order B G R A, as the A8R8G8B8 display format), no CMASK / FMASK / fast clear.
				let rt_lo = (rt[idx] as u64 >> 8) as u32;
				let slice = W * H / 64 - 1;
				c.ctx(0x28c60, &[
					rt_lo,                            // CB_COLOR0_BASE
					W / 8 - 1,                        // CB_COLOR0_PITCH (TILE_MAX)
					slice,                            // CB_COLOR0_SLICE (TILE_MAX)
					0,                                // CB_COLOR0_VIEW
					(0x0a << 2) | (1 << 11) | (1 << 15), // CB_COLOR0_INFO: FORMAT 8_8_8_8, COMP_SWAP ALT, BLEND_CLAMP
					8 | (8 << 5),                     // CB_COLOR0_ATTRIB: TILE_MODE_INDEX 8, FMASK_TILE_MODE_INDEX 8
					0,                                // (reserved)
					rt_lo,                            // CB_COLOR0_CMASK
					0,                                // CB_COLOR0_CMASK_SLICE
					rt_lo,                            // CB_COLOR0_FMASK
					slice,                            // CB_COLOR0_FMASK_SLICE
					0,
					0,                                // CB_COLOR0_CLEAR_WORD0/1
				]);
				c.ctx(0x28238, &[0xf]); // CB_TARGET_MASK
				c.ctx(0x28780, &[0]); // CB_BLEND0_CONTROL: no blending
				c.ctx(0x28808, &[(1 << 4) | (0xcc << 16)]); // CB_COLOR_CONTROL: MODE normal, ROP3 copy
				// No depth or stencil.
				c.ctx(0x28000, &[0]); // DB_RENDER_CONTROL
				c.ctx(0x28040, &[0, 0]); // DB_Z_INFO, DB_STENCIL_INFO
				c.ctx(0x28800, &[0]); // DB_DEPTH_CONTROL
				// Scissors and viewport: the whole buffer; NDC y up.
				let br = (H << 16) | W;
				c.ctx(0x28030, &[0, br]); // PA_SC_SCREEN_SCISSOR_TL/BR
				c.ctx(0x28200, &[0, 0x8000_0000, br]); // PA_SC_WINDOW_OFFSET, PA_SC_WINDOW_SCISSOR_TL/BR
				c.ctx(0x28240, &[0x8000_0000, br]); // PA_SC_GENERIC_SCISSOR_TL/BR
				c.ctx(0x28250, &[0x8000_0000, br]); // PA_SC_VPORT_SCISSOR_0_TL/BR
				c.ctx(0x282d0, &[0f32.to_bits(), 1f32.to_bits()]); // PA_SC_VPORT_ZMIN_0/ZMAX_0
				let (hw, hh) = (W as f32 / 2.0, H as f32 / 2.0);
				c.ctx(0x2843c, &[hw.to_bits(), hw.to_bits(), (-hh).to_bits(), hh.to_bits(), 0.5f32.to_bits(), 0.5f32.to_bits()]); // PA_CL_VPORT_*
				c.ctx(0x28818, &[0x43f]); // PA_CL_VTE_CNTL: viewport scale/offset on, W is 1/W
				c.ctx(0x28810, &[0]); // PA_CL_CLIP_CNTL
				c.ctx(0x28814, &[0]); // PA_SU_SC_MODE_CNTL: no culling
				c.ctx(0x28be0, &[0]); // PA_SC_AA_CONFIG: one sample
				c.ctx(0x28c38, &[0xffff_ffff, 0xffff_ffff]); // PA_SC_AA_MASK_X0Y0_X1Y0 / X0Y1_X1Y1
				c.ctx(0x28644, &[0]); // SPI_PS_INPUT_CNTL_0: PS input 0 = VS parameter 0
				c.ctx(0x28b54, &[0]); // VGT_SHADER_STAGES_EN: plain VS + PS
				c.uconfig(0x30908, &[4]); // VGT_PRIMITIVE_TYPE: triangle list
				c.pkt3(0x2f, &[1]); // NUM_INSTANCES 1
				let r_draw = sceGnmDrawIndexAuto(c.reserve(7), 7, 6, 0);
				// End of pipe: flush and invalidate the colour caches, then write frame + 1 to the label.
				let la = label as u64;
				c.pkt3(0x47, &[0x14 | (5 << 8), la as u32, ((la >> 32) as u32 & 0xffff) | (1 << 29), frame + 1, 0]);
				core::arch::x86_64::_mm_sfence();
				sceGnmFlushGarlic();
				let dcb = [c.p as *mut c_void];
				let bytes = [(c.n * 4) as u32];
				let ts = Instant::now();
				let r_sub = sceGnmSubmitCommandBuffers(1, dcb.as_ptr(), bytes.as_ptr(), core::ptr::null(), core::ptr::null());
				let r_done = sceGnmSubmitDone();
				if frame < 2 || r_sub != 0 {
					log.say(format!(
						"gnm: frame {frame}: {} dwords; init {r_init:#x}, vs {r_vs:#x}, ps {r_ps:#x}, draw {r_draw:#x}, submit {r_sub:#x}, submit done {r_done:#x}",
						c.n
					));
				}
				if r_sub != 0 {
					return;
				}
				// Wait for the GPU.
				let mut ok = false;
				while ts.elapsed() < Duration::from_secs(2) {
					if label.read_volatile() == frame + 1 {
						ok = true;
						break;
					}
					std::thread::sleep(Duration::from_micros(100));
				}
				let ms = ts.elapsed().as_secs_f64() * 1000.0;
				gpu_ms += ms;
				if frame < 2 || !ok {
					let px = |x: u32, y: u32| rt[idx].add((y * W + x) as usize).read_volatile();
					log.say(format!(
						"gnm: frame {frame}: GPU {} after {ms:.2} ms (label {}); pixels: corner {:08x}, centre {:08x}, near top {:08x}",
						if ok { "finished" } else { "DID NOT finish" },
						label.read_volatile(),
						px(10, 10),
						px(W / 2, H / 2),
						px(W / 2, H / 2 - 200)
					));
				}
				if !ok {
					return;
				}
				let rf = sceVideoOutSubmitFlip(h, idx as i32, 1, frame as i64);
				if frame < 2 {
					log.say(format!("gnm: frame {frame}: flip {rf:#x}"));
				}
				let mut ev = [0u64; 4];
				let mut n = 0;
				let mut us = 100_000u32;
				sceKernelWaitEqueue(eq, &mut ev, 1, &mut n, &mut us);
			}
			log.say(format!(
				"gnm: {frames} frames in {:.1} s, submit-to-label {:.2} ms a frame on average",
				t0.elapsed().as_secs_f64(),
				gpu_ms / frames as f64
			));
		}
	}
}
