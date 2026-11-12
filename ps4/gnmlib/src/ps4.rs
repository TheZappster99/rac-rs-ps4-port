//! The console's side: direct memory, the GNM driver calls, video out. PS4 only (target_os = freebsd); link with
//! -lSceGnmDriver -lSceVideoOut (ps4/build.sh).

use core::ffi::{c_char, c_void};

unsafe extern "C" {
	fn sceKernelGetDirectMemorySize() -> usize;
	fn sceKernelAllocateDirectMemory(start: i64, end: i64, len: usize, align: usize, mem_type: i32, phys: *mut i64) -> i32;
	fn sceKernelMapDirectMemory(addr: *mut *mut c_void, len: usize, prot: i32, flags: i32, phys: i64, align: usize) -> i32;
	fn sceKernelReleaseDirectMemory(phys: i64, len: usize) -> i32;
	fn sceKernelCreateEqueue(eq: *mut u64, name: *const c_char) -> i32;
	fn sceKernelWaitEqueue(eq: u64, ev: *mut [u64; 4], n: i32, out: *mut i32, timeout: *mut u32) -> i32;

	fn sceGnmDrawInitDefaultHardwareState350(cmd: *mut u32, n: u32) -> i32;
	fn sceGnmSetVsShader(cmd: *mut u32, n: u32, regs: *const u32, modifier: u32) -> i32;
	fn sceGnmSetPsShader350(cmd: *mut u32, n: u32, regs: *const u32) -> i32;
	fn sceGnmDrawIndexAuto(cmd: *mut u32, n: u32, count: u32, flags: u32) -> i32;
	fn sceGnmSubmitCommandBuffers(count: u32, dcb: *const *mut c_void, dcb_bytes: *const u32, ccb: *const *mut c_void, ccb_bytes: *const u32) -> i32;
	fn sceGnmSubmitDone() -> i32;
	fn sceGnmFlushGarlic();

	fn sceVideoOutOpen(user: i32, bus: i32, index: i32, param: *const c_void) -> i32;
	fn sceVideoOutSetBufferAttribute(attr: *mut [u32; 10], format: u32, tmode: u32, aspect: u32, w: u32, h: u32, pitch: u32);
	fn sceVideoOutRegisterBuffers(h: i32, start: i32, addrs: *const *mut c_void, n: i32, attr: *const [u32; 10]) -> i32;
	fn sceVideoOutSubmitFlip(h: i32, idx: i32, mode: u32, arg: i64) -> i32;
	fn sceVideoOutSetFlipRate(h: i32, rate: i32) -> i32;
	fn sceVideoOutAddFlipEvent(eq: u64, h: i32, udata: *mut c_void) -> i32;
}

/// Write-combined ("garlic") direct memory, CPU and GPU read / write: what every GPU resource here lives in.
const MEM_WC_GARLIC: i32 = 3;
/// Write-back ("onion") direct memory: CPU cached, and the GPU's accesses snoop the CPU caches. For buffers the CPU reads back
/// (MAP_READ): CPU reads of write-combined garlic are uncached, one memory round trip per load.
const MEM_WB_ONION: i32 = 0;
const PROT_CPU_GPU_RW: i32 = 0x33;
/// SCE_KERNEL_MAP_FIXED | SCE_KERNEL_MAP_NO_OVERWRITE: exactly this address, and fail rather than replace a mapping.
const MAP_FIXED_NOREPLACE: i32 = 0x10 | 0x80;

/// A block of direct memory, handed out by a bump allocator. CPU and GPU addresses are the same on the PS4.
pub struct Arena {
	pub base: *mut u8,
	pub len: usize,
	off: usize,
}

impl Arena {
	/// `len` bytes (rounded up to 2 MB), anywhere.
	pub fn new(len: usize) -> Result<Arena, String> {
		Self::map(len, None)
	}

	/// `len` bytes at exactly `addr` (2 MB aligned), or an error. radv's GFX7 shaders read descriptor sets, vertex-buffer
	/// tables and push constants through 32-bit pointers whose high half is 0, so those must live below 4 GB.
	pub fn new_at(len: usize, addr: u64) -> Result<Arena, String> {
		Self::map(len, Some(addr))
	}

	/// `len` bytes of CPU-cached onion memory (rounded up to 2 MB), anywhere: for GPU results the CPU reads.
	pub fn new_onion(len: usize) -> Result<Arena, String> {
		Self::map_type(len, None, MEM_WB_ONION)
	}

	/// `len` bytes with `addr` as a hint only (no MAP_FIXED); accepted when the kernel's choice ends below `limit`.
	pub fn new_below(len: usize, addr: u64, limit: u64) -> Result<Arena, String> {
		let a = Self::map_hint(len, addr)?;
		if a.base as u64 + a.len as u64 <= limit {
			Ok(a)
		} else {
			// The mapping stays (no unmap in this layer yet); it is small and only made during start-up.
			Err(format!("hint {addr:#x} gave {:p}, not below {limit:#x}", a.base))
		}
	}

	fn map_hint(len: usize, hint: u64) -> Result<Arena, String> {
		let align = 2 << 20;
		let len = len.div_ceil(align) * align;
		let mut phys = 0i64;
		// SAFETY: plain libkernel calls with valid out-pointers.
		unsafe {
			let r = sceKernelAllocateDirectMemory(0, sceKernelGetDirectMemorySize() as i64, len, align, MEM_WC_GARLIC, &mut phys);
			if r < 0 {
				return Err(format!("sceKernelAllocateDirectMemory({len}) {r:#x}"));
			}
			let mut addr = hint as *mut c_void;
			let r = sceKernelMapDirectMemory(&mut addr, len, PROT_CPU_GPU_RW, 0, phys, align);
			if r < 0 || addr.is_null() {
				sceKernelReleaseDirectMemory(phys, len);
				return Err(format!("sceKernelMapDirectMemory hint {hint:#x} -> {r:#x}"));
			}
			Ok(Arena { base: addr as *mut u8, len, off: 0 })
		}
	}

	fn map(len: usize, at: Option<u64>) -> Result<Arena, String> {
		Self::map_type(len, at, MEM_WC_GARLIC)
	}

	fn map_type(len: usize, at: Option<u64>, mem_type: i32) -> Result<Arena, String> {
		let align = 2 << 20;
		let len = len.div_ceil(align) * align;
		let mut phys = 0i64;
		// SAFETY: plain libkernel calls with valid out-pointers.
		unsafe {
			let r = sceKernelAllocateDirectMemory(0, sceKernelGetDirectMemorySize() as i64, len, align, mem_type, &mut phys);
			if r < 0 {
				return Err(format!("sceKernelAllocateDirectMemory({len}, type {mem_type}) {r:#x}"));
			}
			let mut addr = at.unwrap_or(0) as *mut c_void;
			let flags = if at.is_some() { MAP_FIXED_NOREPLACE } else { 0 };
			let r = sceKernelMapDirectMemory(&mut addr, len, PROT_CPU_GPU_RW, flags, phys, align);
			if r < 0 || addr.is_null() || at.is_some_and(|a| addr as u64 != a) {
				sceKernelReleaseDirectMemory(phys, len);
				return Err(format!("sceKernelMapDirectMemory at {at:x?} -> {addr:p}, {r:#x}"));
			}
			Ok(Arena { base: addr as *mut u8, len, off: 0 })
		}
	}

	/// `len` bytes aligned to `align`; returns the CPU pointer (= GPU address).
	pub fn alloc(&mut self, len: usize, align: usize) -> *mut u8 {
		let at = self.off.div_ceil(align) * align;
		assert!(at + len <= self.len, "arena full: {} + {len} > {}", at, self.len);
		self.off = at + len;
		// SAFETY: inside the mapping (checked above).
		unsafe { self.base.add(at) }
	}

	/// A zeroed u32 slice of `n` words.
	pub fn words(&mut self, n: usize, align: usize) -> &'static mut [u32] {
		let p = self.alloc(n * 4, align) as *mut u32;
		// SAFETY: fresh, exclusive memory of the mapping, which lives for the program.
		unsafe {
			core::ptr::write_bytes(p, 0, n);
			core::slice::from_raw_parts_mut(p, n)
		}
	}

	/// Copies `bytes` in at `align`; returns the GPU address.
	pub fn upload(&mut self, bytes: &[u8], align: usize) -> u64 {
		let p = self.alloc(bytes.len(), align);
		// SAFETY: fresh memory of the mapping, `bytes.len()` long.
		unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len()) };
		p as u64
	}
}

/// sceGnmDrawInitDefaultHardwareState350 into 256 reserved dwords; returns the driver's result (0x100 = written).
pub fn init_default_state(buf: &mut [u32]) -> i32 {
	assert!(buf.len() >= 256);
	// SAFETY: 256 writable dwords.
	unsafe { sceGnmDrawInitDefaultHardwareState350(buf.as_mut_ptr(), 256) }
}

/// sceGnmSetVsShader into 29 reserved dwords.
pub fn set_vs(buf: &mut [u32], regs: &[u32; 7]) -> i32 {
	assert!(buf.len() >= 29);
	// SAFETY: 29 writable dwords, 7 register words.
	unsafe { sceGnmSetVsShader(buf.as_mut_ptr(), 29, regs.as_ptr(), 0) }
}

/// sceGnmSetPsShader350 into 40 reserved dwords.
pub fn set_ps(buf: &mut [u32], regs: &[u32; 12]) -> i32 {
	assert!(buf.len() >= 40);
	// SAFETY: 40 writable dwords, 12 register words.
	unsafe { sceGnmSetPsShader350(buf.as_mut_ptr(), 40, regs.as_ptr()) }
}

/// sceGnmDrawIndexAuto into 7 reserved dwords: `count` vertices numbered 0..count.
pub fn draw_auto(buf: &mut [u32], count: u32) -> i32 {
	assert!(buf.len() >= 7);
	// SAFETY: 7 writable dwords.
	unsafe { sceGnmDrawIndexAuto(buf.as_mut_ptr(), 7, count, 0) }
}

/// Submits a stream of draw commands (in direct memory) and tells the driver the frame's work is in. Returns
/// (submit result, submit-done result). A stream longer than one command buffer may be (`pm4::MAX_SUBMIT_DWORDS`) goes as
/// several buffers in one call, cut on packet boundaries; the CP runs them in order, with the state carried across.
/// Nanoseconds spent inside the driver calls since the last `take_times`: flush, submit, submit done, flip, the wait for the
/// flip event. The 2026-11-09 console profile showed the main thread blocked in one system library call for about 60% of a
/// 200 ms frame while the GPU had long finished; these name the call (the back end prints them in its stats line).
pub static TIMES: [core::sync::atomic::AtomicU64; 5] = [const { core::sync::atomic::AtomicU64::new(0) }; 5];

fn timed<R>(slot: usize, f: impl FnOnce() -> R) -> R {
	let t = std::time::Instant::now();
	let r = f();
	TIMES[slot].fetch_add(t.elapsed().as_nanos() as u64, core::sync::atomic::Ordering::Relaxed);
	r
}

/// The `TIMES` counters, reset to zero.
pub fn take_times() -> [u64; 5] {
	core::array::from_fn(|i| TIMES[i].swap(0, core::sync::atomic::Ordering::Relaxed))
}

pub fn submit(cmd: &[u32]) -> (i32, i32) {
	// Only a submission over the limit is walked packet by packet to be split: the walk cost the main thread a tenth of
	// its time on the console (2.8 MB of commands a frame), for buffers that were never over the limit.
	let pieces = if cmd.len() <= crate::pm4::MAX_SUBMIT_DWORDS {
		vec![(0, cmd.len())]
	} else {
		match crate::pm4::split(cmd, crate::pm4::MAX_SUBMIT_DWORDS) {
			Ok(p) => p,
			// A header that cannot be sized: submitting it whole would be cut by the CP anyway, so refuse.
			Err(_) => return (-1, 0),
		}
	};
	let dcb: Vec<*mut c_void> = pieces.iter().map(|&(s, _)| cmd[s..].as_ptr() as *mut c_void).collect();
	let bytes: Vec<u32> = pieces.iter().map(|&(_, l)| (l * 4) as u32).collect();
	// The CPU wrote the buffer through write-combining: make the writes visible before the GPU reads.
	// SAFETY: plain driver calls on buffers the GPU can read.
	unsafe {
		core::arch::x86_64::_mm_sfence();
		timed(0, || sceGnmFlushGarlic());
		let r = timed(1, || sceGnmSubmitCommandBuffers(dcb.len() as u32, dcb.as_ptr(), bytes.as_ptr(), core::ptr::null(), core::ptr::null()));
		// sceGnmSubmitDone tells the system the frame's GPU work is all submitted. It used to follow every submission (2-3 a
		// frame); `DONE_PER_FRAME` leaves it to `submit_done`, once a frame before the flip.
		let done = if DONE_PER_FRAME.load(core::sync::atomic::Ordering::Relaxed) { 0 } else { timed(2, || sceGnmSubmitDone()) };
		(r, done)
	}
}

/// Set: `submit` no longer calls sceGnmSubmitDone; the caller does, once a frame (`submit_done`).
pub static DONE_PER_FRAME: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// sceGnmSubmitDone, for `DONE_PER_FRAME`.
pub fn submit_done() -> i32 {
	// SAFETY: a plain driver call.
	timed(2, || unsafe { sceGnmSubmitDone() })
}

/// Waits (up to `timeout_ms`) for the GPU to write `value` to `label`; true if it did.
pub fn wait_label(label: *const u32, value: u32, timeout_ms: u64) -> bool {
	let t = std::time::Instant::now();
	while t.elapsed().as_millis() < timeout_ms as u128 {
		// SAFETY: the label is mapped GPU memory the GPU writes.
		if unsafe { label.read_volatile() } == value {
			return true;
		}
		std::thread::sleep(std::time::Duration::from_micros(100));
	}
	false
}

/// Video out with registered A8R8G8B8 (sRGB) linear buffers.
pub struct Video {
	pub handle: i32,
	eq: u64,
}

impl Video {
	pub fn open(bufs: &[*mut u8], width: u32, height: u32, pitch: u32) -> Result<Video, String> {
		// SAFETY: libSceVideoOut / libkernel calls with valid pointers; the buffers stay mapped for the program.
		unsafe {
			let h = sceVideoOutOpen(0xff, 0, 0, core::ptr::null());
			if h < 0 {
				return Err(format!("sceVideoOutOpen {h:#x}"));
			}
			let mut eq = 0u64;
			sceKernelCreateEqueue(&mut eq, c"rac-rs gnm flip".as_ptr());
			sceVideoOutAddFlipEvent(eq, h, core::ptr::null_mut());
			let mut attr = [0u32; 10];
			sceVideoOutSetBufferAttribute(&mut attr, 0x8000_0000, 1, 0, width, height, pitch);
			let addrs: Vec<*mut c_void> = bufs.iter().map(|&p| p as *mut c_void).collect();
			let r = sceVideoOutRegisterBuffers(h, 0, addrs.as_ptr(), addrs.len() as i32, &attr);
			if r < 0 {
				return Err(format!("sceVideoOutRegisterBuffers {r:#x}"));
			}
			sceVideoOutSetFlipRate(h, 0);
			Ok(Video { handle: h, eq })
		}
	}

	/// Shows buffer `idx` at the next vblank and waits (up to 100 ms) for the flip.
	pub fn flip(&self, idx: usize, arg: i64) -> i32 {
		// SAFETY: driver calls with valid arguments.
		unsafe {
			let r = timed(3, || sceVideoOutSubmitFlip(self.handle, idx as i32, 1, arg));
			let mut ev = [0u64; 4];
			let mut n = 0;
			let mut us = 100_000u32;
			timed(4, || sceKernelWaitEqueue(self.eq, &mut ev, 1, &mut n, &mut us));
			r
		}
	}
}
