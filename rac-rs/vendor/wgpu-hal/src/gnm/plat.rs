//! The console side of the GNM back end: direct memory, the driver's packet helpers, command buffer submission and video
//! out. On the PS4 (`target_os = "freebsd"`) these are `rac_ps4_gnm::ps4`; anywhere else a fake GPU stands in: memory comes
//! from the heap, the driver helpers write NOP packets of the same size, and submitting executes nothing. The fake keeps the
//! whole back end (pipeline lookups in the shader cache, descriptors, command buffers) testable on the PC.

use alloc::string::String;

/// True when there is no real GPU behind the back end.
pub const FAKE: bool = cfg!(not(target_os = "freebsd"));

/// A block of GPU-visible memory. CPU and GPU addresses are the same on the PS4.
pub struct Block {
	pub base: *mut u8,
	pub len: usize,
}

// SAFETY: the block is plain memory owned by the process for its whole life.
unsafe impl Send for Block {}
unsafe impl Sync for Block {}

/// `len` bytes of CPU+GPU read-write memory (write-combined "garlic" on the PS4). `low` = below 4 GB of address space,
/// where radv's GFX7 shaders can reach through 32-bit pointers (descriptor sets, vertex-buffer tables, push constants).
pub fn map(len: usize, low: bool) -> Result<Block, String> {
	#[cfg(target_os = "freebsd")]
	{
		use rac_ps4_gnm::ps4::Arena;
		let a = if low {
			let mut r = Err(String::from("no low address tried"));
			for at in [0x8000_0000u64, 0x4000_0000, 0xc000_0000, 0x2000_0000] {
				r = Arena::new_at(len, at);
				if r.is_ok() {
					break;
				}
			}
			r?
		} else {
			Arena::new(len)?
		};
		Ok(Block { base: a.base, len: a.len })
	}
	#[cfg(not(target_os = "freebsd"))]
	{
		let _ = low;
		// RAC_GNM_SUBMIT_HASH=1: every block at a fixed address (Windows), so the submission hashes depend on the commands
		// only and runs can be compared.
		#[cfg(windows)]
		if std::env::var("RAC_GNM_SUBMIT_HASH").is_ok_and(|v| v == "1") {
			unsafe extern "system" {
				fn VirtualAlloc(addr: *mut u8, size: usize, kind: u32, protect: u32) -> *mut u8;
			}
			static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0x20_0000_0000);
			let at = NEXT.fetch_add(len.next_multiple_of(1 << 30), std::sync::atomic::Ordering::Relaxed);
			// SAFETY: a fresh reservation at an address nothing else uses; MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE.
			let p = unsafe { VirtualAlloc(at as *mut u8, len.max(1), 0x3000, 4) };
			if !p.is_null() {
				return Ok(Block { base: p, len });
			}
		}
		let layout = std::alloc::Layout::from_size_align(len.max(1), 1 << 16).map_err(|e| alloc::format!("{e}"))?;
		// SAFETY: a non-zero size layout; the block lives for the program (never freed, like the console's mappings).
		let p = unsafe { std::alloc::alloc_zeroed(layout) };
		if p.is_null() {
			return Err(alloc::format!("out of memory for {len} bytes"));
		}
		Ok(Block { base: p, len })
	}
}

/// `len` bytes of CPU-cached memory the GPU also reads and writes ("onion" on the PS4: the GPU's accesses snoop the CPU caches),
/// for buffers the CPU reads back. The fake: plain heap memory, as `map`.
pub fn map_cached(len: usize) -> Result<Block, String> {
	#[cfg(target_os = "freebsd")]
	{
		let a = rac_ps4_gnm::ps4::Arena::new_onion(len)?;
		Ok(Block { base: a.base, len: a.len })
	}
	#[cfg(not(target_os = "freebsd"))]
	{
		map(len, false)
	}
}

/// Fills `buf` with one NOP packet (what the fake writes where a driver helper would).
fn nops(buf: &mut [u32]) {
	if buf.is_empty() {
		return;
	}
	if buf.len() == 1 {
		buf[0] = 0x8000_0000;
		return;
	}
	buf[0] = rac_ps4_gnm::pm4::header(rac_ps4_gnm::regs::op::NOP, buf.len() - 1);
	buf[1..].fill(0);
}

/// The driver's default hardware state (256 dwords).
pub fn init_default_state(buf: &mut [u32]) -> i32 {
	#[cfg(target_os = "freebsd")]
	{
		rac_ps4_gnm::ps4::init_default_state(buf)
	}
	#[cfg(not(target_os = "freebsd"))]
	{
		nops(&mut buf[..256]);
		0x100
	}
}

/// sceGnmSetVsShader into 29 dwords.
pub fn set_vs(buf: &mut [u32], regs: &[u32; 7]) -> i32 {
	#[cfg(target_os = "freebsd")]
	{
		rac_ps4_gnm::ps4::set_vs(buf, regs)
	}
	#[cfg(not(target_os = "freebsd"))]
	{
		let _ = regs;
		nops(&mut buf[..29]);
		0
	}
}

/// sceGnmSetPsShader350 into 40 dwords.
pub fn set_ps(buf: &mut [u32], regs: &[u32; 12]) -> i32 {
	#[cfg(target_os = "freebsd")]
	{
		rac_ps4_gnm::ps4::set_ps(buf, regs)
	}
	#[cfg(not(target_os = "freebsd"))]
	{
		let _ = regs;
		nops(&mut buf[..40]);
		0
	}
}

/// sceGnmDrawIndexAuto into 7 dwords.
pub fn draw_auto(buf: &mut [u32], count: u32) -> i32 {
	#[cfg(target_os = "freebsd")]
	{
		rac_ps4_gnm::ps4::draw_auto(buf, count)
	}
	#[cfg(not(target_os = "freebsd"))]
	{
		let _ = count;
		nops(&mut buf[..7]);
		0
	}
}

/// Submits one command buffer that lives in GPU-visible memory. Returns the driver's results (0, 0 = fine).
pub fn submit(cmd: &[u32]) -> (i32, i32) {
	#[cfg(target_os = "freebsd")]
	{
		rac_ps4_gnm::ps4::submit(cmd)
	}
	#[cfg(not(target_os = "freebsd"))]
	{
		// The fake carries out the CP DMA packets (copies and fills) on the CPU, so buffers the GPU would fill by copy (uniforms
		// written through wgpu's staging buffers) hold the same data as on the console: the frame trace's buffer dump then gives
		// a PC reference to compare the console's against.
		// RAC_GNM_SUBMIT_HASH=1: an FNV hash of each submission's words, to check that a back-end change leaves the command
		// stream unchanged (a deterministic program such as ps4/wgputest, run before and after).
		static HASH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
		if *HASH.get_or_init(|| std::env::var("RAC_GNM_SUBMIT_HASH").is_ok_and(|v| v == "1")) {
			let h = cmd.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &w| (h ^ w as u64).wrapping_mul(0x100_0000_01b3));
			std::eprintln!("GNM: submit {} dwords hash {h:016x}", cmd.len());
		}
		// RAC_GNM_FAKE_DMA=0 skips both (the console's GPU does this work), so a PC profile shows only what the console's CPU does.
		static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
		if *OFF.get_or_init(|| std::env::var("RAC_GNM_FAKE_DMA").is_ok_and(|v| v == "0")) {
			return (0, 0);
		}
		fake_dma(cmd);
		// The console splits big submissions on packet boundaries (ps4::submit): check here that every stream can be split.
		match rac_ps4_gnm::pm4::split(cmd, rac_ps4_gnm::pm4::MAX_SUBMIT_DWORDS) {
			Ok(p) if p.len() > 1 => {
				static ONCE: std::sync::Once = std::sync::Once::new();
				ONCE.call_once(|| std::eprintln!("GNM: fake submit of {} dwords split into {} command buffers", cmd.len(), p.len()));
			}
			Ok(_) => {}
			Err(at) => std::eprintln!("GNM: fake submit of {} dwords cannot be split: bad packet header at dword {at} ({:#010x})", cmd.len(), cmd[at.min(cmd.len() - 1)]),
		}
		(0, 0)
	}
}

/// Milliseconds spent in the console's driver calls since the last call: flush, submit, submit done, flip, flip wait
/// (zeros on the PC fake).
pub fn driver_ms() -> [f64; 5] {
	#[cfg(target_os = "freebsd")]
	{
		rac_ps4_gnm::ps4::take_times().map(|ns| ns as f64 / 1e6)
	}
	#[cfg(not(target_os = "freebsd"))]
	{
		[0.0; 5]
	}
}

/// sceGnmSubmitDone once a frame, before the flip: called by present. After every submission (as until round 9) the next
/// sceGnmSubmitCommandBuffers blocked until the GPU had finished the frame so far, which hid the GPU's time inside "submit"
/// and stalled the thread that records the next commands. RAC_GNM_SUBMIT_DONE=submit brings the old way back.
pub fn frame_done() {
	#[cfg(target_os = "freebsd")]
	{
		static PER_FRAME: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
		let on = *PER_FRAME.get_or_init(|| {
			let on = !std::env::var("RAC_GNM_SUBMIT_DONE").is_ok_and(|v| v == "submit");
			rac_ps4_gnm::ps4::DONE_PER_FRAME.store(on, core::sync::atomic::Ordering::Relaxed);
			if !on {
				std::eprintln!("GNM: sceGnmSubmitDone after every submission (RAC_GNM_SUBMIT_DONE=submit)");
			}
			on
		});
		if on {
			rac_ps4_gnm::ps4::submit_done();
		}
	}
}

/// Video out: buffers registered as the display's, flipped by index.
pub struct Video {
	#[cfg(target_os = "freebsd")]
	v: rac_ps4_gnm::ps4::Video,
}

impl Video {
	pub fn open(bufs: &[*mut u8], width: u32, height: u32, pitch: u32) -> Result<Video, String> {
		#[cfg(target_os = "freebsd")]
		{
			Ok(Video { v: rac_ps4_gnm::ps4::Video::open(bufs, width, height, pitch)? })
		}
		#[cfg(not(target_os = "freebsd"))]
		{
			let _ = (bufs, width, height, pitch);
			Ok(Video {})
		}
	}

	/// Shows buffer `idx` at the next vertical blank and waits for it (up to 100 ms).
	pub fn flip(&self, idx: usize, arg: i64) -> i32 {
		#[cfg(target_os = "freebsd")]
		{
			self.v.flip(idx, arg)
		}
		#[cfg(not(target_os = "freebsd"))]
		{
			let _ = (idx, arg);
			0
		}
	}
}

// SAFETY: video out is used from one thread at a time (the queue's present), behind the surface's lock.
unsafe impl Send for Video {}
unsafe impl Sync for Video {}

/// Forces the run log (stderr, which the PS4 app sends to /data/rac-rs/log.txt) to disk. A console that freezes or is
/// switched off at the button keeps only what was written out: the first console run came back with an empty log.
pub fn sync_log() {
	#[cfg(target_os = "freebsd")]
	{
		unsafe extern "C" {
			// ps4/std.patch: fsync of the log file std's stderr writes to (the PS4 app hands it over at start).
			fn rac_std_sync_log() -> i32;
		}
		// SAFETY: no arguments; -1 when no log file was handed over.
		unsafe { rac_std_sync_log() };
	}
}

/// After a GPU hang: on the console, end the process (abort) once the log is on disk, rather than keep feeding a hung GPU
/// (the first console run froze the whole console). The PC's fake GPU never hangs, so there it does nothing.
pub fn fatal() {
	#[cfg(target_os = "freebsd")]
	{
		if std::env::var("RAC_GNM_HANG_ABORT").is_ok_and(|v| v == "0") {
			return;
		}
		std::eprintln!("GNM: ending the app after the GPU hang (RAC_GNM_HANG_ABORT=0 in env.txt keeps it running)");
		sync_log();
		std::process::abort();
	}
}

/// The fake GPU's CP DMA: DMA_DATA packets (opcode 0x50, as `CommandEncoder::dma` writes them) copied or filled on the CPU.
/// GPU addresses are CPU pointers into the heaps here.
#[cfg(not(target_os = "freebsd"))]
fn fake_dma(cmd: &[u32]) {
	let mut at = 0;
	while at < cmd.len() {
		let h = cmd[at];
		let len = match h >> 30 {
			0 | 3 => ((h >> 16) & 0x3fff) as usize + 2,
			2 => 1,
			_ => return,
		};
		if h >> 30 == 3 && (h >> 8) & 0xff == 0x50 && len == 7 && at + 7 <= cmd.len() {
			let b = &cmd[at + 1..at + 7];
			let sel = (b[0] >> 29) & 3;
			let dst = b[3] as u64 | (b[4] as u64) << 32;
			let n = (b[5] & 0x1f_ffff) as usize;
			// SAFETY: the back end only writes DMA packets for ranges inside its own heap allocations.
			unsafe {
				if sel == 2 {
					let p = dst as *mut u32;
					for i in 0..n / 4 {
						p.add(i).write_unaligned(b[1]);
					}
				} else if sel == 0 || sel == 3 {
					// 3 = through L2 (`RAC_GNM_DMA_L2`): the same copy.
					let src = b[1] as u64 | (b[2] as u64) << 32;
					core::ptr::copy(src as *const u8, dst as *mut u8, n);
				}
			}
		}
		at += len;
	}
}
