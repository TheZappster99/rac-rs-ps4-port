//! rac-rs PS4 port: the GNM back end (PS4_GPU_DESIGN.md, "wgpu on GNM"). With the `ps4-gnm` feature this module takes the
//! place of the `noop` back end, so wgpu and wgpu-core drive it unchanged (`Backends::NOOP`, `NoopBackendOptions::enable`).
//!
//! It draws on the PS4's GPU (AMD GCN GFX7) through the GNM layer (`ps4/gnmlib`): direct memory for every resource, PM4
//! command buffers written here, the driver only for its packet helpers and submission, video out for the surface. Shaders
//! come compiled from the shipped shader cache (`pipe`). Off the console a fake GPU stands in (`plat::FAKE`): everything is
//! built and recorded but nothing executes, which tests the back end and the cache on the PC.
//!
//! First version, kept simple on purpose:
//!   - Every texture is linear aligned (depth: 1D tiled). Mipmapped colour textures use the hardware's power-of-two mip
//!     layout so a view can sample the whole chain; depth views see one mip level (the base of their range).
//!   - Barriers drain the GPU (end-of-pipe flush, wait, cache invalidate), only when work was recorded since the last one.
//!   - Clears and copies use the command processor's DMA; clears of 8 / 16-byte texels need a 4-byte repeating value.
//!   - No MSAA, queries, timestamps, indirect draws or ray tracing (logged once when asked for).

#![allow(unused_variables)]

use alloc::{string::String, sync::Arc, vec, vec::Vec};
use core::{
	ptr,
	sync::atomic::{AtomicU32, AtomicU64, Ordering},
	time::Duration,
};
use std::collections::HashSet;

use parking_lot::Mutex;
use rac_ps4_gnm::desc;

use crate::TlasInstance;

mod blit;
mod command;
mod format;
mod mem;
mod pipe;
mod plat;
mod shot;

pub use command::{CommandBuffer, CommandEncoder};
use format::Fmt;
use mem::{Alloc, Heap};

#[derive(Clone, Debug)]
pub struct Api;

type DeviceResult<T> = Result<T, crate::DeviceError>;

/// Heap sizes. The PS4 app has ~4.6 GB of direct memory. The fake GPU takes the same sizes, so a PC run shows whether a program
/// fits the console's budget (its old 512 MB was too small for rac-rs loading a level).
const MAIN_HEAP: usize = 2048 << 20;
const LOW_HEAP: usize = 64 << 20;

/// What the device, queue, encoders and surface share.
pub struct Shared {
	pub main: Heap,
	pub low: Heap,
	pub cache: pipe::ShaderCache,
	/// The barrier label (8 bytes) and its counter.
	pub barrier_label: Alloc,
	pub barrier_count: AtomicU32,
	/// 256 zero bytes: the scratch-ring table radv's shaders get in user SGPRs 0-1 (no scratch yet).
	pub zeros: Alloc,
	/// The do-nothing pixel shader (code) for pipelines without a fragment stage.
	pub noop_ps: Alloc,
	/// The programs of the back end's own clear / copy draws (blit.rs), at `blit::*_AT`; made on first use, so runs without
	/// blits keep every other allocation (and the fake GPU's command stream hash) as before.
	blit: std::sync::OnceLock<Option<Alloc>>,
	/// `RAC_GNM_BLIT_CHECK`: this frame's blit checks, compared at present.
	pub blit_checks: Mutex<Vec<blit::Check>>,
	/// Command buffer memory of submissions, freed once their fence label reaches the value.
	pending: Mutex<Vec<(*const AtomicU64, u64, Alloc)>>,
	/// The last submission's fence (for present).
	last_submit: Mutex<Option<(*const AtomicU64, u64)>>,
	/// Things logged once (unsupported features), by name.
	once: Mutex<HashSet<&'static str>>,
	/// Frame timing and submission sizes for the run log (`note_frame`).
	stats: Mutex<Stats>,
	/// Draws and dispatches skipped because their pipeline is missing from the cache or skip-listed (`RAC_GNM_SKIP`).
	pub skipped: AtomicU64,
	/// CP DMA bytes recorded since the last stats line: fills, copies (the console moves ~2.7 GB/s this way, round 10).
	pub dma_bytes: [AtomicU64; 2],
	/// Uploads that found the low heap full (their packets point at zeros instead: wrong pictures, possibly a GPU fault).
	pub low_full: AtomicU64,
	/// Frames shown so far (for `ab_mode`).
	pub shown: AtomicU64,
	/// Pipelines ever submitted, and the ones first submitted in each of the last submissions (newest last), for the GPU
	/// hang report: a hang right after a pipeline's first use points at it.
	seen: Mutex<(HashSet<u64>, alloc::collections::VecDeque<(u64, Vec<u64>)>)>,
	/// The display buffers' addresses (for the frame trace: "this target is the screen").
	pub surface_addrs: Mutex<Vec<u64>>,
	/// Frame trace: buffers to dump once the GPU has finished the frame (what, address, size).
	pub trace_bufs: Mutex<Vec<(String, u64, u64)>>,
	/// Frame trace: raw upload files written so far (their number is in the name).
	pub trace_files: AtomicU32,
	/// Frame trace: the middle row of each pass's first colour target, copied by the CP after the pass into this block, and
	/// the records (what, offset in the block, texels, texel bytes, format) printed once the frame is done.
	trace_snap: Mutex<(Option<Alloc>, usize, Vec<(String, usize, u32, u32, wgt::TextureFormat)>)>,
	/// GPU profile (`gpu_prof`): the block the GPU writes its clock into at each mark, and each mark's name by slot.
	gpu_prof: Mutex<(Option<Alloc>, Vec<String>)>,
	/// The scratch ring (made on first use, `scratch_tmpring`): its memory and the table radv's shaders read through user SGPRs
	/// 0-1 (AC_UD_SCRATCH_RING_OFFSETS: the ring's base address and swizzle bit; ACO builds the rest of the descriptor).
	scratch: Mutex<Option<(Alloc, Alloc)>>,
	/// The readback heap (`readback_heap`): CPU-cached memory for MAP_READ buffers, made on the first one.
	readback: std::sync::OnceLock<Option<Heap>>,
	/// `RAC_GNM_CS_TRACE=1` (console diagnostics): the label each compute dispatch writes once it has finished (made on first use), the
	/// next dispatch id, and the last dispatches recorded (id, encoder, pipeline key, grid, the address of group 0's first dynamic buffer).
	cs_label: std::sync::OnceLock<Option<Alloc>>,
	cs_next: AtomicU32,
	cs_ring: Mutex<alloc::collections::VecDeque<(u32, usize, u64, [u32; 3], u64)>>,
}

/// The readback heap's size: the GS capture's two pictures (4 + 12 MB) and the small timing / self-check buffers fit with room.
const READBACK_HEAP: usize = 64 << 20;

/// Scratch ring: waves that can hold scratch at once, and the most bytes one wave may use (the ring is their product, 32 MB).
/// The console's first level hung the GPU on the one cached shader that spills (`sky model`'s fs_reduce, 72 KB a wave), which
/// got the zero table instead of a ring.
const SCRATCH_WAVES: u32 = 256;
const SCRATCH_WAVE_MAX: u32 = 128 << 10;

/// What the run log's once-a-second `GNM: stats` line reports (console diagnostics: how fast, GPU or CPU bound, how big a
/// frame is, how full the heaps are).
struct Stats {
	start: std::time::Instant,
	since: std::time::Instant,
	frames: u64,
	/// In the current one-second window: frames, the time present waited for the GPU (sum, most), submissions, their dwords
	/// (sum, most), and the skipped-draw count at the window's start.
	n: u64,
	wait_sum: Duration,
	wait_max: Duration,
	submits: u64,
	words_sum: u64,
	words_max: usize,
	skipped_at: u64,
	/// In the window, with the flip on its own thread (`Flipper`): the time that thread waited for the GPU to finish a frame
	/// (sum, frames). The main thread's "present waited" is then only the wait for the previous frame's flip.
	gpu_sum: Duration,
	gpu_n: u64,
	/// Presents in the window that waited for the GPU themselves (a diagnostic needed the frame's results, or `RAC_GNM_PRESENT=wait`).
	sync_n: u64,
}

impl Stats {
	fn new() -> Stats {
		let now = std::time::Instant::now();
		Stats { start: now, since: now, frames: 0, n: 0, wait_sum: Duration::ZERO, wait_max: Duration::ZERO, submits: 0, words_sum: 0, words_max: 0, skipped_at: 0, gpu_sum: Duration::ZERO, gpu_n: 0, sync_n: 0 }
	}
}

// SAFETY: the raw label pointers in `pending` / `last_submit` point at fence labels, which are never freed (see `Fence`).
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

impl core::fmt::Debug for Shared {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		f.write_str("gnm::Shared")
	}
}

impl Shared {
	fn new() -> Result<Shared, String> {
		let main = Heap::new("main", MAIN_HEAP, false)?;
		let low = Heap::new("low", LOW_HEAP, true)?;
		let barrier_label = low.alloc(8, 8).ok_or("no memory for the barrier label")?;
		let zeros = low.alloc(256, 256).ok_or("no memory")?;
		let noop_ps = main.alloc(256, 256).ok_or("no memory")?;
		// SAFETY: fresh allocations.
		unsafe {
			barrier_label.words().fill(0);
			zeros.words().fill(0);
			noop_ps.words()[0] = pipe::NOOP_PS_CODE[0];
		}
		log::info!(
			"GNM: {} back end; main heap {} MB at {:#x}, low heap {} MB at {:#x}",
			if plat::FAKE { "fake-GPU" } else { "PS4" },
			MAIN_HEAP >> 20,
			main.base(),
			LOW_HEAP >> 20,
			low.base()
		);
		Ok(Shared {
			main,
			low,
			cache: pipe::ShaderCache::load(),
			barrier_label,
			barrier_count: AtomicU32::new(0),
			zeros,
			noop_ps,
			blit: std::sync::OnceLock::new(),
			blit_checks: Mutex::new(Vec::new()),
			pending: Mutex::new(Vec::new()),
			last_submit: Mutex::new(None),
			once: Mutex::new(HashSet::new()),
			stats: Mutex::new(Stats::new()),
			skipped: AtomicU64::new(0),
			dma_bytes: [AtomicU64::new(0), AtomicU64::new(0)],
			low_full: AtomicU64::new(0),
			shown: AtomicU64::new(0),
			seen: Mutex::new((HashSet::new(), alloc::collections::VecDeque::new())),
			surface_addrs: Mutex::new(Vec::new()),
			trace_bufs: Mutex::new(Vec::new()),
			trace_files: AtomicU32::new(0),
			trace_snap: Mutex::new((None, 0, Vec::new())),
			gpu_prof: Mutex::new((None, Vec::new())),
			scratch: Mutex::new(None),
			readback: std::sync::OnceLock::new(),
			cs_label: std::sync::OnceLock::new(),
			cs_next: AtomicU32::new(1),
			cs_ring: Mutex::new(alloc::collections::VecDeque::new()),
		})
	}

	/// `RAC_GNM_CS_TRACE=1`: the dispatch label (8 bytes in the low heap, made on first use), or None when the switch is off. Every compute
	/// dispatch is then followed by a drain that writes its id there, so a GPU hang report names the last dispatch that finished and the
	/// ones recorded after it (`cs_note`, `gpu_hung`). Slow (a full drain per dispatch): diagnostics only.
	pub fn cs_trace(&self) -> Option<u64> {
		self.cs_label
			.get_or_init(|| {
				if !std::env::var("RAC_GNM_CS_TRACE").is_ok_and(|v| v == "1") {
					return None;
				}
				let a = self.low.alloc(8, 8)?;
				// SAFETY: a fresh allocation.
				unsafe { a.words().fill(0) };
				std::eprintln!("GNM: RAC_GNM_CS_TRACE: a drain after every compute dispatch, the hang report names the dispatch");
				Some(a)
			})
			.as_ref()
			.map(|a| a.addr())
	}

	/// A dispatch recorded under `cs_trace`: its id (what the label holds once it has finished), encoder, pipeline, grid, params address.
	pub fn cs_note(&self, enc: usize, key: u64, grid: [u32; 3], params: u64) -> u32 {
		let id = self.cs_next.fetch_add(1, Ordering::Relaxed);
		let mut r = self.cs_ring.lock();
		if r.len() >= 256 {
			r.pop_front();
		}
		r.push_back((id, enc, key, grid, params));
		id
	}

	/// The hang report's dispatch part (`cs_trace` on): the last finished dispatch and the next ones recorded in the same encoder.
	fn cs_report(&self) {
		let Some(Some(label)) = self.cs_label.get() else { return };
		// SAFETY: the label lives for the program; the GPU writes it, so a volatile read.
		let done = unsafe { core::ptr::read_volatile(label.ptr as *const u32) };
		let r = self.cs_ring.lock();
		let enc = r.iter().find(|e| e.0 == done).map(|e| e.1);
		std::eprintln!("GNM: CS trace: last finished dispatch id {done} (encoder {enc:x?}); {} recorded, ids {}..{}", r.len(), r.front().map_or(0, |e| e.0), r.back().map_or(0, |e| e.0));
		// The likely culprit: the first dispatch after the finished one in the same encoder (ids are given at record time, and encoders
		// record side by side, so "after" lines of other encoders may simply not have been submitted yet).
		let hung = r.iter().find(|e| e.0 > done && (enc.is_none() || Some(e.1) == enc)).map(|e| e.0);
		for &(id, e, key, grid, params) in r.iter().filter(|e| e.0 + 3 >= done).take(12) {
			let p: Vec<String> = if params != 0 {
				// SAFETY: the address of a live uniform buffer in the main heap (256 bytes bound); read-only.
				(0..16).map(|i| alloc::format!("{:x}", unsafe { core::ptr::read_volatile((params as *const u32).add(i)) })).collect()
			} else {
				Vec::new()
			};
			std::eprintln!(
				"GNM: CS trace: {} id {id} encoder {e:x} pipeline {key:016x} grid {grid:?} params {}",
				if id <= done { "done" } else if Some(id) == hung { "HUNG? (next in that encoder)" } else { "after" },
				p.join(" ")
			);
		}
	}

	/// The heap MAP_READ buffers go into: CPU-cached ("onion") memory on the console, where reading write-combined garlic back is
	/// uncached (the GS capture reads 4-12 MB a published picture). Made on the first such buffer, so runs without readbacks keep
	/// every other allocation as before. On by default on the console; `RAC_GNM_ONION=0` keeps readbacks in the main heap, and the
	/// fake GPU uses it only with `RAC_GNM_ONION=1` (its submission hashes stay those of older builds).
	fn readback_heap(&self) -> Option<&Heap> {
		self.readback
			.get_or_init(|| {
				let on = match std::env::var("RAC_GNM_ONION") {
					Ok(v) => v != "0",
					Err(_) => !plat::FAKE,
				};
				if !on {
					return None;
				}
				match Heap::new_cached("readback", READBACK_HEAP) {
					Ok(h) => {
						std::eprintln!("GNM: readback heap {} MB at {:#x} (CPU-cached onion memory for MAP_READ buffers)", READBACK_HEAP >> 20, h.base());
						Some(h)
					}
					Err(e) => {
						std::eprintln!("GNM: no readback heap ({e}); MAP_READ buffers stay in the main heap");
						None
					}
				}
			})
			.as_ref()
	}

	/// Logs `msg` the first time `what` happens.
	pub fn once(&self, what: &'static str, msg: impl FnOnce() -> String) {
		if self.once.lock().insert(what) {
			// Both: rac-rs installs no logger for wgpu-hal, its run log is stderr; the ps4/ test programs log through `log`.
			let m = msg();
			std::eprintln!("GNM: {m}");
			log::warn!("GNM: {m}");
		}
	}

	/// Frees submission memory whose fence has passed.
	fn reclaim(&self) {
		let mut p = self.pending.lock();
		p.retain(|&(label, value, a)| {
			// SAFETY: fence labels are never freed.
			let done = unsafe { (*label).load(Ordering::Acquire) } >= value;
			if done {
				self.main.free(a);
			}
			!done
		});
	}

	/// Main-heap memory, reclaiming finished submissions first if the heap is full.
	pub fn alloc_main(&self, len: usize, align: usize) -> DeviceResult<Alloc> {
		if let Some(a) = self.main.alloc(len, align) {
			return Ok(a);
		}
		self.reclaim();
		self.main.alloc(len, align).ok_or_else(|| {
			std::eprintln!("GNM: main heap full ({len} bytes asked, {} free, high-water {} MB)", self.main.free_bytes(), self.main.peak_bytes() >> 20);
			crate::DeviceError::OutOfMemory
		})
	}

	pub fn alloc_low(&self, len: usize, align: usize) -> DeviceResult<Alloc> {
		self.low.alloc(len, align).ok_or_else(|| {
			// Once per 1000: a full low heap tends to stay full, and every draw would print.
			let n = self.low_full.fetch_add(1, Ordering::Relaxed);
			if n % 1000 == 0 {
				std::eprintln!(
					"GNM: low heap full ({len} bytes asked, {} free, high-water {} MB; {} times so far)",
					self.low.free_bytes(),
					self.low.peak_bytes() >> 20,
					n + 1
				);
			}
			crate::DeviceError::OutOfMemory
		})
	}

	/// Console diagnostics, `RAC_GNM_AB=1`: an A/B test inside one run, for a picture that comes out wrong. While the frame
	/// count is in one of these windows, pipelines bound are drawn with blending off (bit 0) and / or the depth and stencil
	/// tests off (bit 1): frames 45-59 no blending, 60-74 no depth test, 75-89 neither. Pictures of frames 40, 55, 70 and 85
	/// (`RAC_PS4_SHOT`) then show which one changes what is missing. The first console run's level scene was still from
	/// frame 30 to 100, so all four windows see the same scene.
	pub fn ab_mode(&self) -> u32 {
		static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
		if !*ON.get_or_init(|| std::env::var("RAC_GNM_AB").is_ok_and(|v| v == "1")) {
			return 0;
		}
		match self.shown.load(Ordering::Relaxed) + 1 {
			45..=59 => 1,
			60..=74 => 2,
			75..=89 => 3,
			_ => 0,
		}
	}

	/// Notes which pipelines a submission uses for the first time (the last 8 submissions that had any are kept).
	fn note_pipelines(&self, cbs: &[&CommandBuffer]) {
		let mut g = self.seen.lock();
		let (seen, recent) = &mut *g;
		let new: Vec<u64> = cbs.iter().flat_map(|c| c.used.iter().copied()).filter(|k| seen.insert(*k)).collect();
		if !new.is_empty() {
			// `RAC_GNM_LOG_FIRST=1`: every first use goes to the log at once (to compare two runs' pipelines on the PC fake).
			static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
			if *LOG.get_or_init(|| std::env::var("RAC_GNM_LOG_FIRST").is_ok_and(|v| v == "1")) {
				let list: Vec<String> = new.iter().map(|k| alloc::format!("{k:016x}")).collect();
				std::eprintln!("GNM: first used in frame {}: {}", self.shown.load(Ordering::Relaxed) + 1, list.join(" "));
			}
			recent.push_back((self.shown.load(Ordering::Relaxed) + 1, new));
			if recent.len() > 8 {
				recent.pop_front();
			}
		}
	}

	/// Console diagnostics, `RAC_GNM_TRACE=40,300`: while those frames are recorded, every render / compute pass, its targets
	/// (marked SCREEN when it is a display buffer) and every texture copy goes to the log, to see how a frame reaches the screen.
	pub fn tracing(&self) -> Option<u64> {
		static F: std::sync::OnceLock<Vec<u64>> = std::sync::OnceLock::new();
		let f = F.get_or_init(|| std::env::var("RAC_GNM_TRACE").map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_default());
		let n = self.shown.load(Ordering::Relaxed) + 1;
		f.contains(&n).then_some(n)
	}

	/// Console diagnostics, `RAC_GNM_GPUPROF=700,900`: where the GPU's time goes in those frames. Each pass asks for a mark at
	/// its start ("between passes": the copies and waits before it) and its end (the pass's label); the mark is an end-of-pipe
	/// event that makes the GPU write its clock counter into a slot. The GPU works through a frame in order, so the gap
	/// before each mark is what that pass cost. Returns the slot's address, `None` outside the chosen frames.
	pub fn gpu_prof_mark(&self, what: &str) -> Option<u64> {
		static F: std::sync::OnceLock<pipe::Frames> = std::sync::OnceLock::new();
		let f = F.get_or_init(|| pipe::Frames::env("RAC_GNM_GPUPROF"));
		if !f.has(self.shown.load(Ordering::Relaxed) + 1) {
			return None;
		}
		const SLOTS: usize = 4096;
		let mut g = self.gpu_prof.lock();
		if g.0.is_none() {
			g.0 = self.main.alloc(SLOTS * 8, 256);
			// SAFETY: a fresh allocation of that size.
			unsafe { core::ptr::write_bytes(g.0?.ptr, 0, SLOTS * 8) };
		}
		let slot = g.1.len();
		if slot >= SLOTS {
			return None;
		}
		g.1.push(String::from(what));
		Some(g.0?.addr() + slot as u64 * 8)
	}

	/// GPU profile: prints the finished frame's marks as a table (share of the frame and clock ticks by pass) and clears them.
	fn print_gpu_prof(&self) {
		let mut g = self.gpu_prof.lock();
		let Some(a) = g.0 else { return };
		let names = core::mem::take(&mut g.1);
		if names.is_empty() {
			return;
		}
		// SAFETY: inside the block; the GPU has finished the frame that wrote it.
		let slots = unsafe { core::slice::from_raw_parts_mut(a.ptr as *mut u64, names.len()) };
		let mut marks: Vec<(u64, &str)> = slots.iter().zip(&names).filter(|(t, _)| **t != 0).map(|(&t, n)| (t, n.as_str())).collect();
		let unwritten = names.len() - marks.len();
		slots.fill(0);
		marks.sort();
		let (Some(first), Some(last)) = (marks.first().map(|m| m.0), marks.last().map(|m| m.0)) else { return };
		let total = (last - first).max(1);
		let mut by: Vec<(&str, u64, u32)> = Vec::new();
		for w in marks.windows(2) {
			let dt = w[1].0 - w[0].0;
			match by.iter_mut().find(|b| b.0 == w[1].1) {
				Some(b) => {
					b.1 += dt;
					b.2 += 1;
				}
				None => by.push((w[1].1, dt, 1)),
			}
		}
		by.sort_by(|a, b| b.1.cmp(&a.1));
		let frame = self.shown.load(Ordering::Relaxed);
		std::eprintln!(
			"GNM gpu profile frame {frame}: {} marks ({unwritten} never written), {total} clock ticks from the first to the last, first at {first}, {:.1} ms after start",
			marks.len(),
			self.stats.lock().start.elapsed().as_secs_f64() * 1e3
		);
		for (what, dt, n) in by.iter().take(40) {
			std::eprintln!("GNM gpu profile frame {frame}: {:5.1}% {dt:>12} ticks, {n:>3}x {what}", *dt as f64 * 100.0 / total as f64);
		}
	}

	/// The TMPRING_SIZE value for a pipeline whose shaders use `per_wave` scratch bytes a wave (0 for none), making the ring on
	/// first use; `None` (pipeline treated as missing, logged) when it needs more than the ring holds or the ring cannot be made.
	/// `RAC_GNM_NO_SCRATCH=1` (diagnostics) treats every spilling pipeline as missing instead.
	pub fn scratch_tmpring(&self, per_wave: u32, key: u64, label: crate::Label<'_>) -> Option<u32> {
		if per_wave == 0 {
			return Some(0);
		}
		let refuse = |why: &str| {
			std::eprintln!("GNM: pipeline {key:016x} ({}) needs {per_wave} B of scratch a wave: {why}; its draws are skipped", label.unwrap_or("-"));
			None
		};
		if std::env::var("RAC_GNM_NO_SCRATCH").is_ok_and(|v| v == "1") {
			return refuse("RAC_GNM_NO_SCRATCH=1");
		}
		if per_wave > SCRATCH_WAVE_MAX {
			return refuse("more than the scratch ring allows");
		}
		let mut g = self.scratch.lock();
		if g.is_none() {
			let ring = self.main.alloc((SCRATCH_WAVES * SCRATCH_WAVE_MAX) as usize, 256);
			let table = self.low.alloc(64, 64);
			let (Some(ring), Some(table)) = (ring, table) else {
				return refuse("no memory for the scratch ring");
			};
			let va = ring.addr();
			// SAFETY: a fresh allocation.
			unsafe {
				let t = table.words();
				t.fill(0);
				// BASE_ADDRESS, BASE_ADDRESS_HI | SWIZZLE_ENABLE (GFX6-8), as radv's preamble writes ring slot 0.
				t[0] = va as u32;
				t[1] = (va >> 32) as u32 & 0xffff | 1 << 31;
			}
			std::eprintln!("GNM: scratch ring {} MB at {va:#x} ({SCRATCH_WAVES} waves x {} KB), first for pipeline {key:016x} ({})", (SCRATCH_WAVES * SCRATCH_WAVE_MAX) >> 20, SCRATCH_WAVE_MAX >> 10, label.unwrap_or("-"));
			*g = Some((ring, table));
		}
		Some(SCRATCH_WAVES | per_wave.div_ceil(1024) << 12)
	}

	/// The scratch ring table's address (the zero table before any spilling pipeline was made).
	pub fn scratch_table(&self) -> u64 {
		self.scratch.lock().as_ref().map_or(self.zeros.addr(), |(_, t)| t.addr())
	}

	/// Frame trace: room for one snapshot row of `bytes`; its GPU address, recorded under `what`.
	pub fn snap_slot(&self, what: String, texels: u32, texel_bytes: u32, format: wgt::TextureFormat) -> Option<u64> {
		const SNAP: usize = 4 << 20;
		let bytes = (texels * texel_bytes) as usize;
		let mut g = self.trace_snap.lock();
		if g.0.is_none() {
			g.0 = self.main.alloc(SNAP, 256);
		}
		let base = g.0?.addr();
		let off = g.1;
		if off + bytes > SNAP {
			return None;
		}
		g.1 = (off + bytes).next_multiple_of(256);
		g.2.push((what, off, texels, texel_bytes, format));
		Some(base + off as u64)
	}

	/// The GPU address of the blit programs (blit.rs), written on first use.
	pub fn blit_code(&self) -> u64 {
		let a = self.blit.get_or_init(|| {
			let a = self.main.alloc(blit::CODE_BYTES, 256)?;
			// SAFETY: a fresh allocation of that size.
			blit::write_code(unsafe { a.words() });
			Some(a)
		});
		a.map_or(0, |a| a.addr())
	}

	/// A slot of `bytes` in the snapshot block that `print_snaps` does not print (blit checks); freed with the frame's rows.
	pub fn snap_raw(&self, bytes: usize) -> Option<u64> {
		const SNAP: usize = 4 << 20;
		let mut g = self.trace_snap.lock();
		if g.0.is_none() {
			g.0 = self.main.alloc(SNAP, 256);
		}
		let base = g.0?.addr();
		let off = g.1;
		if off + bytes > SNAP {
			return None;
		}
		g.1 = (off + bytes).next_multiple_of(256);
		Some(base + off as u64)
	}

	/// Frame trace: prints the snapshot rows of the finished frame and empties the list.
	fn print_snaps(&self) {
		let mut g = self.trace_snap.lock();
		let Some(a) = g.0 else { return };
		for (what, off, texels, tb, format) in core::mem::take(&mut g.2) {
			// SAFETY: inside the snapshot block, written by the CP of a finished frame.
			let row = unsafe { core::slice::from_raw_parts(a.ptr.add(off), (texels * tb) as usize) };
			let line = if tb == 4 {
				let px: Vec<&[u8]> = row.chunks(4).collect();
				let lit = px.iter().filter(|p| p[0] | p[1] | p[2] != 0).count();
				let mut sum = [0u64; 4];
				for p in &px {
					for c in 0..4 {
						sum[c] += p[c] as u64;
					}
				}
				let n = px.len().max(1) as u64;
				alloc::format!("{lit} of {} texels not black, mean bytes {} {} {} {}", px.len(), sum[0] / n, sum[1] / n, sum[2] / n, sum[3] / n)
			} else {
				let nz = row.chunks(4).filter(|w| w.iter().any(|&b| b != 0)).count();
				alloc::format!("{nz} of {} words non-zero", row.len() / 4)
			};
			std::eprintln!("GNM trace row {what} ({format:?}): {line}");
		}
		g.1 = 0;
	}

	/// "SCREEN" when `addr` is a display buffer.
	pub fn screen_mark(&self, addr: u64) -> &'static str {
		if self.surface_addrs.lock().contains(&addr) { " SCREEN" } else { "" }
	}

	/// Counts one submission of `words` dwords.
	fn note_submit(&self, words: usize) {
		let mut s = self.stats.lock();
		s.submits += 1;
		s.words_sum += words as u64;
		s.words_max = s.words_max.max(words);
		drop(s);
		if words > rac_ps4_gnm::pm4::MAX_SUBMIT_DWORDS {
			self.once("big-submit", || {
				let n = words.div_ceil(rac_ps4_gnm::pm4::MAX_SUBMIT_DWORDS);
				alloc::format!("a submission of {words} dwords ({} MB) goes as about {n} command buffers (one may hold under 4 MB)", words >> 18)
			});
		}
	}

	/// Counts one presented frame that waited `wait` for the GPU. The first frames and then one line a second go to the log,
	/// which is then forced to disk (a console that freezes loses whatever is still in its cache).
	fn note_frame(&self, wait: Duration) {
		self.shown.fetch_add(1, Ordering::Relaxed);
		let mut s = self.stats.lock();
		s.frames += 1;
		s.n += 1;
		s.wait_sum += wait;
		s.wait_max = s.wait_max.max(wait);
		if s.frames <= 5 {
			std::eprintln!(
				"GNM: frame {} shown, {:.1} s after start; present waited {:.1} ms for the GPU",
				s.frames,
				s.start.elapsed().as_secs_f64(),
				wait.as_secs_f64() * 1e3
			);
			plat::sync_log();
		}
		let dt = s.since.elapsed();
		if dt < Duration::from_secs(1) {
			return;
		}
		let skipped = self.skipped.load(Ordering::Relaxed);
		let n = s.n.max(1);
		// Where the driver itself kept the calling thread, a frame (the console only).
		let d = plat::driver_ms().map(|ms| ms / n as f64);
		if d.iter().any(|&ms| ms > 0.0) {
			std::eprintln!("GNM: driver calls a frame: flush {:.1} ms, submit {:.1} ms, submit done {:.1} ms, flip {:.1} ms, flip wait {:.1} ms", d[0], d[1], d[2], d[3], d[4]);
		}
		// CP DMA a frame, and what it costs the GPU at the console's measured ~2.7 GB/s (the "post fog cover clear" pass, round 10).
		let (fill, copy) = (self.dma_bytes[0].swap(0, Ordering::Relaxed), self.dma_bytes[1].swap(0, Ordering::Relaxed));
		let mb = |b: u64| b as f64 / n as f64 / (1 << 20) as f64;
		std::eprintln!("GNM: CP DMA a frame: fills {:.1} MB, copies {:.1} MB (~{:.1} ms of GPU at 2.7 GB/s)", mb(fill), mb(copy), (mb(fill) + mb(copy)) / 2.7 / 1.024);
		std::eprintln!(
			"GNM: stats {:.0}s: {:.1} fps, present waited for the GPU {:.1} ms avg / {:.1} max, {:.1} submits a frame, {} KB of commands a frame ({} KB biggest), low heap {} of {} MB (peak {}), main {} of {} MB (peak {}), {} draws skipped{}",
			s.start.elapsed().as_secs_f64(),
			s.n as f64 / dt.as_secs_f64(),
			s.wait_sum.as_secs_f64() * 1e3 / n as f64,
			s.wait_max.as_secs_f64() * 1e3,
			s.submits as f64 / n as f64,
			s.words_sum / n / 256,
			s.words_max / 256,
			self.low.used_bytes() >> 20,
			LOW_HEAP >> 20,
			self.low.peak_bytes() >> 20,
			self.main.used_bytes() >> 20,
			MAIN_HEAP >> 20,
			self.main.peak_bytes() >> 20,
			skipped - s.skipped_at,
			match self.low_full.load(Ordering::Relaxed) {
				0 => String::new(),
				f => alloc::format!(", LOW HEAP WAS FULL {f} times"),
			}
		);
		if s.gpu_n > 0 {
			std::eprintln!(
				"GNM: frames in flight: the flip thread waited {:.1} ms a frame for the GPU after present returned ({} frames, {} presented waiting)",
				s.gpu_sum.as_secs_f64() * 1e3 / s.gpu_n as f64,
				s.gpu_n,
				s.sync_n
			);
		}
		s.skipped_at = skipped;
		s.since = std::time::Instant::now();
		(s.n, s.wait_sum, s.wait_max, s.submits, s.words_sum, s.words_max, s.gpu_sum, s.gpu_n, s.sync_n) = (0, Duration::ZERO, Duration::ZERO, 0, 0, 0, Duration::ZERO, 0, 0);
		drop(s);
		plat::sync_log();
	}

	/// The GPU stopped finishing work: log what is known, force the log to disk, and on the console end the app (a hung GPU
	/// takes the whole console with it; ending the process is the best chance the system gets to reset it).
	fn gpu_hung(&self, what: &str) {
		let s = self.stats.lock();
		std::eprintln!(
			"GNM: GPU HANG: {what} did not finish in 2 s; {} frames shown, {:.1} s after start, this second's biggest submission {} KB, low heap {} MB used, {} low-heap-full uploads",
			s.frames,
			s.start.elapsed().as_secs_f64(),
			s.words_max / 256,
			self.low.used_bytes() >> 20,
			self.low_full.load(Ordering::Relaxed)
		);
		drop(s);
		// The pipelines first used shortly before (keys; their labels are in build-ps4/capture/<key>.rec).
		for (frame, keys) in self.seen.lock().1.iter() {
			let list: Vec<String> = keys.iter().map(|k| alloc::format!("{k:016x}")).collect();
			std::eprintln!("GNM: first used in frame {frame}: {}", list.join(" "));
		}
		self.cs_report();
		plat::sync_log();
		plat::fatal();
	}

	/// Something that reads what the GPU left in memory is waiting for the frame being presented (blit checks, snapshot rows,
	/// the GPU profile, trace dumps): that present must wait for the GPU itself, as before frames were kept in flight.
	fn diag_pending(&self) -> bool {
		!self.blit_checks.lock().is_empty() || !self.trace_snap.lock().2.is_empty() || !self.gpu_prof.lock().1.is_empty() || !self.trace_bufs.lock().is_empty()
	}

	/// Waits for `label` to reach `value`, at most 2 s; false (after `gpu_hung`) if it did not.
	fn wait_label(&self, label: *const AtomicU64, value: u64, what: &str) -> bool {
		let t = std::time::Instant::now();
		// SAFETY: fence labels are never freed.
		while unsafe { (*label).load(Ordering::Acquire) } < value {
			if t.elapsed() > Duration::from_secs(2) {
				self.gpu_hung(what);
				return false;
			}
			std::thread::yield_now();
		}
		true
	}
}

// ---------------------------------------------------------------------------------------------------------------------
// Objects.

#[derive(Debug)]
pub struct Instance {
	options: Arc<wgt::NoopBackendOptions>,
}

#[derive(Debug)]
pub struct Adapter {
	options: Arc<wgt::NoopBackendOptions>,
}

#[derive(Debug)]
pub struct Device {
	shared: Arc<Shared>,
}

#[derive(Debug)]
pub struct Queue {
	shared: Arc<Shared>,
}

/// Unused resource kinds (queries, acceleration structures, pipeline caches).
#[derive(Debug)]
pub struct Resource;

/// A GPU memory range. Mapping returns the pointer itself (unified, coherent memory).
#[derive(Debug)]
pub struct Buffer {
	pub(crate) alloc: Option<Alloc>,
	pub(crate) size: u64,
}

impl Buffer {
	pub fn addr(&self) -> u64 {
		self.alloc.map_or(0, |a| a.addr())
	}
}

/// One mip level of a texture: offset from the texture's base, pitch in texels, bytes per array layer.
#[derive(Clone, Copy, Debug)]
pub struct Level {
	pub offset: u64,
	pub pitch: u32,
	pub width: u32,
	pub height: u32,
	pub layer_bytes: u64,
	/// The stencil plane of a depth-stencil format: offset of this level's plane (pitch as the depth plane, 1 byte texels).
	pub stencil_offset: u64,
	pub stencil_layer_bytes: u64,
}

#[derive(Debug)]
pub struct Texture {
	/// The memory, or `None` for a surface texture (the surface owns it).
	alloc: Option<Alloc>,
	pub(crate) addr: u64,
	pub(crate) format: wgt::TextureFormat,
	pub(crate) fmt: Fmt,
	pub(crate) dimension: wgt::TextureDimension,
	/// Array layers, or depth for 3D textures.
	pub(crate) layers: u32,
	pub(crate) levels: Vec<Level>,
	/// Mipmapped colour texture in the hardware's mip layout (`format::mip_pitch`).
	pub(crate) chain: bool,
	/// DEV: filled with word indices for wgputest's mip address probe (`RAC_GNM_FILL_OFFSETS=1`); uploads are dropped.
	pub(crate) probe: bool,
	/// For surface textures: the video out buffer index.
	pub(crate) surface_index: Option<usize>,
}

impl Texture {
	/// GPU address of (level, layer).
	pub fn subresource(&self, level: u32, layer: u32) -> u64 {
		let l = &self.levels[level as usize];
		self.addr + l.offset + l.layer_bytes * layer as u64
	}

	pub fn stencil_subresource(&self, level: u32, layer: u32) -> u64 {
		let l = &self.levels[level as usize];
		self.addr + l.stencil_offset + l.stencil_layer_bytes * layer as u64
	}

	pub fn is_depth(&self) -> bool {
		self.fmt.depth.is_some()
	}
}

/// A texture view: one mip level (the base of the range) and a range of layers.
#[derive(Clone, Copy, Debug)]
pub struct TextureView {
	pub(crate) addr: u64,
	pub(crate) stencil_addr: u64,
	pub(crate) format: wgt::TextureFormat,
	pub(crate) fmt: Fmt,
	pub(crate) level: Level,
	pub(crate) layers: u32,
	/// The image descriptor (radv's 64-byte sampled-image slot takes it, then 32 zero bytes).
	pub(crate) image: [u32; 8],
	pub(crate) depth_aspect_only: bool,
}

#[derive(Debug)]
pub struct Sampler {
	pub(crate) words: [u32; 4],
}

#[derive(Debug)]
pub struct BindGroupLayout {
	pub(crate) key: u64,
	pub(crate) entries: Arc<[wgt::BindGroupLayoutEntry]>,
	/// radv's layout, slots by wgpu binding number; `None` if no cached pipeline uses this layout.
	pub(crate) layout: Option<Arc<pipe::SetLayout>>,
}

#[derive(Debug)]
pub struct PipelineLayout {
	pub(crate) key: u64,
	/// Each group's number of dynamic buffers (radv numbers them across the layout, group by group).
	pub(crate) dynamic_counts: Vec<u32>,
	pub(crate) immediate_size: u32,
}

/// A descriptor set in the low heap; dynamic buffers kept apart for `set_bind_group`'s offsets.
#[derive(Debug)]
pub struct BindGroup {
	pub(crate) mem: Option<Alloc>,
	/// (GPU address, bound size) of each dynamic buffer, in the group's dynamic order.
	pub(crate) dynamic: Vec<(u64, u32)>,
	/// (binding, GPU address, size) of each plain buffer binding (frame trace: the buffer dump).
	pub(crate) bufs: Vec<(u32, u64, u64)>,
	shared: Arc<Shared>,
}

impl Drop for BindGroup {
	fn drop(&mut self) {
		if let Some(a) = self.mem.take() {
			self.shared.low.free(a);
		}
	}
}

#[derive(Debug)]
pub struct ShaderModule {
	pub(crate) naga: Option<crate::NagaShader>,
}

/// A graphics pipeline from the cache. `None` inside = missing from the cache: draws are skipped.
#[derive(Debug)]
pub struct RenderPipeline {
	pub(crate) inner: Option<Arc<command::Gfx>>,
}

#[derive(Debug)]
pub struct ComputePipeline {
	pub(crate) inner: Option<Arc<command::Comp>>,
}

/// A fence: a 64-bit label the GPU writes at the end of each submission (EVENT_WRITE_EOP). Labels are never freed, so a
/// submission still in flight can always write its label (they are 8 bytes each).
#[derive(Debug)]
pub struct Fence {
	label: *const AtomicU64,
}

// SAFETY: the label is never freed and only accessed atomically.
unsafe impl Send for Fence {}
unsafe impl Sync for Fence {}

impl Fence {
	fn value(&self) -> u64 {
		// SAFETY: see the type.
		unsafe { (*self.label).load(Ordering::Acquire) }
	}
}

/// The display: three textures registered with video out, flipped in turn.
#[derive(Debug)]
pub struct Surface {
	state: Mutex<Option<SurfaceState>>,
}

struct SurfaceState {
	video: Arc<plat::Video>,
	textures: Vec<(Alloc, Level)>,
	format: wgt::TextureFormat,
	next: usize,
	flips: i64,
	/// The flip thread, made at the first present that keeps a frame in flight.
	flipper: Option<Flipper>,
}

/// One frame in flight. `present` used to wait for the GPU to finish the frame before flipping it, so the main thread could
/// not record the next frame while the GPU drew this one: CPU and GPU took turns (console, round 15: ~50 ms of CPU, then
/// ~50 ms of GPU, ~10 fps). Now `present` hands the frame to this thread, which waits for the GPU and flips; the main thread
/// goes on with the next frame and waits only for the previous flip. With three display buffers that is always safe: one
/// shown, one waiting to be flipped, one being drawn. Memory the GPU still reads is fenced already (submissions, upload
/// chunks and wgpu-core's staging are freed by fence). `RAC_GNM_PRESENT=wait` brings back the wait in `present`.
struct Flipper {
	tx: std::sync::mpsc::Sender<FlipJob>,
	/// The flip count of the last frame flipped (or given up after a GPU hang).
	done: Arc<(std::sync::Mutex<i64>, std::sync::Condvar)>,
	/// The flip count of the last job sent. A frame presented the old way (a diagnostic frame) moves the surface's count on
	/// without a job, so waiting must go by what was sent, not by the surface's count.
	sent: i64,
}

struct FlipJob {
	/// The frame's last fence label (an address: labels are never freed) and value; 0 for no submission.
	label: usize,
	value: u64,
	idx: usize,
	flips: i64,
}

impl Flipper {
	fn start(shared: Arc<Shared>, video: Arc<plat::Video>) -> Flipper {
		let (tx, rx) = std::sync::mpsc::channel::<FlipJob>();
		let done = Arc::new((std::sync::Mutex::new(0i64), std::sync::Condvar::new()));
		let d = done.clone();
		let spawned = std::thread::Builder::new().name(String::from("gnm-flip")).spawn(move || {
			while let Ok(job) = rx.recv() {
				let t = std::time::Instant::now();
				let ok = job.label == 0 || shared.wait_label(job.label as *const AtomicU64, job.value, "a frame (flip thread)");
				let waited = t.elapsed();
				if ok {
					let r = video.flip(job.idx, job.flips);
					if r != 0 {
						log::warn!("GNM: flip {} returned {r:#x}", job.idx);
					}
					let mut s = shared.stats.lock();
					s.gpu_sum += waited;
					s.gpu_n += 1;
				}
				*d.0.lock().unwrap() = job.flips;
				d.1.notify_all();
			}
		});
		if let Err(e) = spawned {
			std::eprintln!("GNM: could not start the flip thread: {e}");
		}
		Flipper { tx, done, sent: 0 }
	}

	/// Waits until the last frame sent (and every one before it) has been flipped.
	fn wait(&self) {
		let mut g = self.done.0.lock().unwrap();
		while *g < self.sent {
			g = self.done.1.wait(g).unwrap();
		}
	}
}

/// `RAC_GNM_PRESENT=wait`: present waits for the GPU itself and flips on the calling thread (the old way).
fn present_waits() -> bool {
	static W: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
	*W.get_or_init(|| {
		let w = std::env::var("RAC_GNM_PRESENT").is_ok_and(|v| v == "wait");
		std::eprintln!("GNM: present {}", if w { "waits for the GPU (RAC_GNM_PRESENT=wait)" } else { "keeps one frame in flight (flip thread)" });
		w
	})
}

impl core::fmt::Debug for SurfaceState {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		f.write_str("SurfaceState")
	}
}

impl crate::Api for Api {
	const VARIANT: wgt::Backend = wgt::Backend::Noop;

	type Instance = Instance;
	type Surface = Surface;
	type Adapter = Adapter;
	type Device = Device;

	type Queue = Queue;
	type CommandEncoder = CommandEncoder;
	type CommandBuffer = CommandBuffer;

	type Buffer = Buffer;
	type Texture = Texture;
	type SurfaceTexture = Texture;
	type TextureView = TextureView;
	type Sampler = Sampler;
	type QuerySet = Resource;
	type Fence = Fence;
	type AccelerationStructure = Resource;
	type PipelineCache = Resource;

	type BindGroupLayout = BindGroupLayout;
	type BindGroup = BindGroup;
	type PipelineLayout = PipelineLayout;
	type ShaderModule = ShaderModule;
	type RenderPipeline = RenderPipeline;
	type RayTracingPipeline = Resource;
	type ComputePipeline = ComputePipeline;
}

crate::impl_dyn_resource!(
	Instance,
	Adapter,
	Device,
	Queue,
	Surface,
	Resource,
	Buffer,
	Texture,
	TextureView,
	Sampler,
	BindGroupLayout,
	PipelineLayout,
	BindGroup,
	ShaderModule,
	RenderPipeline,
	ComputePipeline,
	Fence,
	CommandEncoder,
	CommandBuffer
);

impl crate::DynAccelerationStructure for Resource {}
impl crate::DynBindGroup for BindGroup {}
impl crate::DynBindGroupLayout for BindGroupLayout {}
impl crate::DynBuffer for Buffer {}
impl crate::DynCommandBuffer for CommandBuffer {}
impl crate::DynComputePipeline for ComputePipeline {}
impl crate::DynFence for Fence {}
impl crate::DynPipelineCache for Resource {}
impl crate::DynPipelineLayout for PipelineLayout {}
impl crate::DynQuerySet for Resource {}
impl crate::DynRenderPipeline for RenderPipeline {}
impl crate::DynRayTracingPipeline for Resource {}
impl crate::DynSampler for Sampler {}
impl crate::DynShaderModule for ShaderModule {}
impl crate::DynSurfaceTexture for Texture {}
impl crate::DynTexture for Texture {}
impl crate::DynTextureView for TextureView {}

impl core::borrow::Borrow<dyn crate::DynTexture> for Texture {
	fn borrow(&self) -> &dyn crate::DynTexture {
		self
	}
}

// ---------------------------------------------------------------------------------------------------------------------
// Instance and adapter.

impl crate::Instance for Instance {
	type A = Api;

	unsafe fn init(desc: &crate::InstanceDescriptor<'_>) -> Result<Self, crate::InstanceError> {
		let options = Arc::new(desc.backend_options.noop.clone());
		if options.enable {
			Ok(Instance { options })
		} else {
			Err(crate::InstanceError::new(String::from("GNM back end disabled (NoopBackendOptions::enable is false)")))
		}
	}

	unsafe fn create_surface(
		&self,
		_display_handle: raw_window_handle::RawDisplayHandle,
		_window_handle: raw_window_handle::RawWindowHandle,
	) -> Result<Surface, crate::InstanceError> {
		// The PS4 has one display; any handle stands for it.
		Ok(Surface { state: Mutex::new(None) })
	}

	unsafe fn enumerate_adapters(&self, _surface_hint: Option<&Surface>) -> Vec<crate::ExposedAdapter<Api>> {
		let info = wgt::AdapterInfo {
			name: String::from(if plat::FAKE { "PS4 GPU (GNM, fake)" } else { "PS4 GPU (GNM)" }),
			driver: String::from("rac-rs GNM"),
			..wgt::AdapterInfo::new(wgt::DeviceType::IntegratedGpu, wgt::Backend::Noop)
		};
		vec![crate::ExposedAdapter {
			adapter: Adapter { options: Arc::clone(&self.options) },
			info,
			features: features(),
			capabilities: capabilities(),
		}]
	}
}

/// Features offered: what the renderer uses and this back end handles.
fn features() -> wgt::Features {
	use wgt::Features as F;
	F::IMMEDIATES
		| F::DEPTH32FLOAT_STENCIL8
		| F::INDIRECT_FIRST_INSTANCE
		| F::TEXTURE_FORMAT_16BIT_NORM
		| F::FLOAT32_FILTERABLE
		| F::RG11B10UFLOAT_RENDERABLE
		| F::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES
		| F::DEPTH_CLIP_CONTROL
		| F::CLEAR_TEXTURE
		// rac-rs writes @early_depth_test(force) into its fog-storing shaders when this is offered (viewer/volfog/soft.rs), and
		// the cache was captured on an adapter that offers it: without it every pipeline of the shared level shader got another
		// key. radv compiles it for GFX7; the setting travels in the cached DB_SHADER_CONTROL.
		| F::SHADER_EARLY_DEPTH_TEST
}

fn capabilities() -> crate::Capabilities {
	let limits = wgt::Limits {
		max_texture_dimension_1d: 16384,
		max_texture_dimension_2d: 16384,
		max_texture_dimension_3d: 2048,
		max_texture_array_layers: 2048,
		max_bind_groups: 8,
		max_bindings_per_bind_group: 1000,
		max_dynamic_uniform_buffers_per_pipeline_layout: 16,
		max_dynamic_storage_buffers_per_pipeline_layout: 8,
		max_sampled_textures_per_shader_stage: 64,
		max_samplers_per_shader_stage: 32,
		max_storage_buffers_per_shader_stage: 32,
		max_storage_textures_per_shader_stage: 16,
		max_uniform_buffers_per_shader_stage: 32,
		max_uniform_buffer_binding_size: 64 << 10,
		max_storage_buffer_binding_size: 1 << 30,
		max_buffer_size: 1 << 30,
		max_vertex_buffers: 16,
		max_vertex_attributes: 32,
		max_immediate_size: 128,
		max_compute_workgroup_storage_size: 32768,
		max_compute_invocations_per_workgroup: 1024,
		max_compute_workgroup_size_x: 1024,
		max_compute_workgroup_size_y: 1024,
		max_compute_workgroup_size_z: 64,
		max_compute_workgroups_per_dimension: 65535,
		..wgt::Limits::default()
	};
	crate::Capabilities {
		limits,
		alignments: crate::Alignments {
			buffer_copy_offset: wgt::BufferSize::new(4).unwrap(),
			buffer_copy_pitch: wgt::BufferSize::new(4).unwrap(),
			uniform_bounds_check_alignment: wgt::BufferSize::new(16).unwrap(),
			raw_tlas_instance_size: 0,
			ray_tracing_scratch_buffer_alignment: 1,
			ray_tracing_pipeline_group_data_size: 1,
			ray_tracing_pipeline_group_data_alignment: 1,
			ray_tracing_pipeline_data_offset_alignment: 1,
		},
		downlevel: wgt::DownlevelCapabilities {
			flags: wgt::DownlevelFlags::all(),
			limits: wgt::DownlevelLimits {},
			shader_model: wgt::ShaderModel::Sm5,
		},
		cooperative_matrix_properties: Vec::new(),
	}
}

/// The display's size: what the surface reports and its textures default to.
const DISPLAY: (u32, u32) = (1920, 1080);

impl crate::Adapter for Adapter {
	type A = Api;

	unsafe fn open(&self, features: wgt::Features, _limits: &wgt::Limits, _memory_hints: &wgt::MemoryHints) -> DeviceResult<crate::OpenDevice<Api>> {
		let shared = Arc::new(Shared::new().map_err(|e| {
			log::error!("GNM: {e}");
			crate::DeviceError::OutOfMemory
		})?);
		Ok(crate::OpenDevice { device: Device { shared: Arc::clone(&shared) }, queue: Queue { shared } })
	}

	unsafe fn texture_format_capabilities(&self, format: wgt::TextureFormat) -> crate::TextureFormatCapabilities {
		use crate::TextureFormatCapabilities as C;
		let Some(f) = format::of(format) else {
			return C::empty();
		};
		let mut c = C::SAMPLED | C::COPY_SRC | C::COPY_DST;
		if f.depth.is_some() {
			c |= C::DEPTH_STENCIL_ATTACHMENT;
		} else {
			c |= C::SAMPLED_LINEAR | C::COLOR_ATTACHMENT;
			// Integer formats do not blend.
			if !matches!(f.img_num, 4 | 5) {
				c |= C::COLOR_ATTACHMENT_BLEND;
			}
			if matches!(f.bytes, 4 | 8 | 16) {
				c |= C::STORAGE_READ_ONLY | C::STORAGE_WRITE_ONLY | C::STORAGE_READ_WRITE;
			}
		}
		c
	}

	unsafe fn surface_capabilities(&self, _surface: &Surface) -> Option<crate::SurfaceCapabilities> {
		let formats = [wgt::TextureFormat::Bgra8UnormSrgb, wgt::TextureFormat::Bgra8Unorm]
			.into_iter()
			.map(|format| wgt::SurfaceFormatCapabilities { format, color_spaces: wgt::SurfaceColorSpaces::SRGB })
			.collect();
		Some(crate::SurfaceCapabilities {
			formats,
			maximum_frame_latency: 1..=2,
			current_extent: Some(wgt::Extent3d { width: DISPLAY.0, height: DISPLAY.1, depth_or_array_layers: 1 }),
			// COPY_SRC: rac-rs configures its swapchain with it (screenshots read the frame back). The buffers are plain linear
			// direct memory, so a copy out of them is an ordinary texture copy.
			usage: wgt::TextureUses::COLOR_TARGET | wgt::TextureUses::COPY_DST | wgt::TextureUses::COPY_SRC,
			present_modes: vec![wgt::PresentMode::Fifo],
			composite_alpha_modes: vec![wgt::CompositeAlphaMode::Opaque],
		})
	}

	unsafe fn get_presentation_timestamp(&self) -> wgt::PresentationTimestamp {
		wgt::PresentationTimestamp::INVALID_TIMESTAMP
	}

	fn get_ordered_buffer_usages(&self) -> wgt::BufferUses {
		wgt::BufferUses::INCLUSIVE | wgt::BufferUses::MAP_WRITE
	}

	fn get_ordered_texture_usages(&self) -> wgt::TextureUses {
		wgt::TextureUses::INCLUSIVE | wgt::TextureUses::COLOR_TARGET | wgt::TextureUses::DEPTH_STENCIL_WRITE
	}
}

// ---------------------------------------------------------------------------------------------------------------------
// Surface.

impl Surface {
	/// Makes the three display buffers and registers them with video out (`st` is empty).
	unsafe fn configure_new(&self, device: &Device, config: &crate::SurfaceConfiguration, st: &mut Option<SurfaceState>) -> Result<(), crate::SurfaceError> {
		let (w, h) = (config.extent.width, config.extent.height);
		format::of(config.format).ok_or(crate::SurfaceError::Other("surface format"))?;
		let pitch = format::pitch_texels(w, 4, false);
		let bytes = pitch as u64 * h as u64 * 4;
		let mut textures = Vec::new();
		for _ in 0..3 {
			let a = device.shared.alloc_main(bytes as usize, 64 << 10).map_err(|_| crate::SurfaceError::Other("no memory for the surface"))?;
			let level = Level { offset: 0, pitch, width: w, height: h, layer_bytes: bytes, stencil_offset: 0, stencil_layer_bytes: 0 };
			textures.push((a, level));
			device.shared.surface_addrs.lock().push(a.addr());
		}
		let ptrs: Vec<*mut u8> = textures.iter().map(|t| t.0.ptr).collect();
		let video = plat::Video::open(&ptrs, w, h, pitch).map_err(|e| {
			log::error!("GNM: video out: {e}");
			crate::SurfaceError::Other("video out")
		})?;
		log::info!("GNM: surface {w}x{h} {:?}, pitch {pitch}", config.format);
		*st = Some(SurfaceState { video: Arc::new(video), textures, format: config.format, next: 0, flips: 0, flipper: None });
		Ok(())
	}
}

impl crate::Surface for Surface {
	type A = Api;

	unsafe fn configure(&self, device: &Device, config: &crate::SurfaceConfiguration) -> Result<(), crate::SurfaceError> {
		let mut st = self.state.lock();
		let (w, h) = (config.extent.width, config.extent.height);
		if let Some((ow, oh)) = st.as_ref().map(|s| (s.textures[0].1.width, s.textures[0].1.height)) {
			// Video out keeps the buffers it registered: the same size reuses them, another size is not supported.
			if (ow, oh) == (w, h) {
				return Ok(());
			}
			// A PC window changes size (the game sizes it after start); the fake GPU has no video out, so it simply makes new
			// buffers. Waiting for the GPU first is not needed: the fake executes nothing.
			if plat::FAKE {
				let old = st.take().unwrap();
				for (a, _) in old.textures {
					device.shared.main.free(a);
				}
				return unsafe { self.configure_new(device, config, &mut *st) };
			}
			log::error!("GNM: the surface was configured for {ow}x{oh}; it cannot change size");
			return Err(crate::SurfaceError::Other("surface size change"));
		}
		unsafe { self.configure_new(device, config, &mut *st) }
	}

	unsafe fn unconfigure(&self, device: &Device) {}

	unsafe fn acquire_texture(&self, _timeout: Option<Duration>, _fence: &Fence) -> Result<crate::AcquiredSurfaceTexture<Api>, crate::SurfaceError> {
		let mut st = self.state.lock();
		let s = st.as_mut().ok_or(crate::SurfaceError::Other("surface not configured"))?;
		let idx = s.next;
		s.next = (s.next + 1) % s.textures.len();
		let (a, level) = s.textures[idx];
		let texture = Texture {
			alloc: None,
			addr: a.addr(),
			format: s.format,
			fmt: format::of(s.format).unwrap(),
			dimension: wgt::TextureDimension::D2,
			layers: 1,
			levels: vec![level],
			chain: false,
			probe: false,
			surface_index: Some(idx),
		};
		Ok(crate::AcquiredSurfaceTexture { texture, suboptimal: false })
	}

	unsafe fn discard_texture(&self, texture: Texture) {}
}

// ---------------------------------------------------------------------------------------------------------------------
// Queue.

impl crate::Queue for Queue {
	type A = Api;

	unsafe fn submit(
		&self,
		command_buffers: &[&CommandBuffer],
		surface_textures: &[&Texture],
		(fence, fence_value): (&Fence, crate::FenceValue),
	) -> DeviceResult<()> {
		self.shared.reclaim();
		let body: usize = command_buffers.iter().map(|c| c.words.len()).sum();
		// Default state and cache invalidate first, the fence label last.
		let total = 256 + 8 + body + 16;
		let a = self.shared.alloc_main(total * 4, 256)?;
		// SAFETY: a fresh allocation of `total` words.
		let words = unsafe { core::slice::from_raw_parts_mut(a.ptr as *mut u32, total) };
		let mut c = rac_ps4_gnm::pm4::Cmd::new(words);
		plat::init_default_state(c.reserve(256));
		c.invalidate_shader_caches();
		for cb in command_buffers {
			let dst = c.reserve(cb.words.len());
			dst.copy_from_slice(&cb.words);
		}
		command::eop_label64(&mut c, fence.label as u64, fence_value);
		let n = c.len();
		self.shared.note_submit(n);
		self.shared.note_pipelines(command_buffers);
		let (r, rd) = plat::submit(&words[..n]);
		if r != 0 {
			log::error!("GNM: submit failed: {r:#x} (submit done {rd:#x})");
			self.shared.main.free(a);
			return Err(crate::DeviceError::Lost);
		}
		if plat::FAKE {
			// Nothing runs: the work is "done" now.
			// SAFETY: fence labels are never freed.
			unsafe { (*fence.label).store(fence_value, Ordering::Release) };
		}
		self.shared.pending.lock().push((fence.label, fence_value, a));
		*self.shared.last_submit.lock() = Some((fence.label, fence_value));
		Ok(())
	}

	unsafe fn present(&self, surface: &Surface, texture: Texture) -> Result<(), crate::SurfaceError> {
		let t = std::time::Instant::now();
		let last = *self.shared.last_submit.lock();
		// Frame in flight (`Flipper`), unless a diagnostic must read this frame's results now or a frame is to be saved.
		{
			let mut st = surface.state.lock();
			let s = st.as_mut().ok_or(crate::SurfaceError::Other("surface not configured"))?;
			let sync = present_waits() || self.shared.diag_pending() || shot::wanted(s.flips as u64 + 1) || self.shared.tracing().is_some();
			// The previous frame's flip first: then its buffer is on screen and the one before it free to draw into.
			if let Some(f) = &s.flipper {
				f.wait();
			}
			if !sync {
				self.shared.note_frame(t.elapsed());
				let idx = texture.surface_index.unwrap_or(0);
				s.flips += 1;
				plat::frame_done();
				let shared = self.shared.clone();
				let video = s.video.clone();
				let f = s.flipper.get_or_insert_with(|| Flipper::start(shared, video));
				let (label, value) = last.map_or((0, 0), |(l, v)| (l as usize, v));
				f.sent = s.flips;
				if f.tx.send(FlipJob { label, value, idx, flips: s.flips }).is_err() {
					// No flip thread (it could not start): flip here, as before.
					if label != 0 && !self.shared.wait_label(label as *const AtomicU64, value, "a frame") {
						return Err(crate::SurfaceError::Lost);
					}
					s.video.flip(idx, s.flips);
					*f.done.0.lock().unwrap() = s.flips;
				}
				return Ok(());
			}
		}
		self.shared.stats.lock().sync_n += 1;
		// The picture must be drawn before it is shown: wait for the last submission.
		if let Some((label, value)) = last {
			if !self.shared.wait_label(label, value, "a frame") {
				return Err(crate::SurfaceError::Lost);
			}
		}
		self.shared.note_frame(t.elapsed());
		blit::print_checks(core::mem::take(&mut *self.shared.blit_checks.lock()));
		self.shared.print_snaps();
		self.shared.print_gpu_prof();
		// Frame trace: the buffers its passes read first, as the GPU left them (unified memory: the CPU reads them directly).
		let dumps: Vec<(String, u64, u64)> = core::mem::take(&mut *self.shared.trace_bufs.lock());
		for (what, addr, size) in dumps {
			let n = (size.min(1 << 16) / 4) as usize;
			// SAFETY: a live buffer's range in the heap's mapping, and the GPU is done with the frame that used it.
			let w = unsafe { core::slice::from_raw_parts(addr as *const u32, n) };
			let nonzero = w.iter().filter(|&&x| x != 0).count();
			// Texels as hex words (RGBA8: AABBGGRR), sampled across the whole range; buffers as floats from the start.
			let head: Vec<String> = if what.contains("upload") {
				(0..16).map(|i| alloc::format!("{:08x}", w[(i * n / 16).min(n.saturating_sub(1))])).collect()
			} else {
				w.iter().take(24).map(|&x| alloc::format!("{:.3}", f32::from_bits(x))).collect()
			};
			std::eprintln!("GNM trace dump {what} at {addr:x}, {size} B, {nonzero} of {n} words non-zero: {}", head.join(" "));
			// Uploads are also saved whole (trace_NNN.raw next to the pictures), to turn into a picture on the PC.
			if what.contains("upload") {
				// SAFETY: as above, the whole range.
				let all = unsafe { core::slice::from_raw_parts(addr as *const u8, size as usize) }.to_vec();
				let i = self.shared.trace_files.fetch_add(1, Ordering::Relaxed);
				let dir: String = std::env::var("RAC_PS4_SHOT_DIR").unwrap_or_else(|_| if cfg!(target_os = "freebsd") { "/data/rac-rs".into() } else { ".".into() });
				let path = alloc::format!("{dir}/trace_{i:03}.raw");
				match std::fs::write(&path, &all) {
					Ok(()) => std::eprintln!("GNM trace dump {what} saved as {path}"),
					Err(e) => std::eprintln!("GNM trace dump {what}: could not save {path}: {e}"),
				}
			}
		}
		let mut st = surface.state.lock();
		let s = st.as_mut().ok_or(crate::SurfaceError::Other("surface not configured"))?;
		let idx = texture.surface_index.unwrap_or(0);
		if let Some(f) = self.shared.tracing() {
			std::eprintln!("GNM trace {f}: present buffer {idx} at {:x}", s.textures[idx].0.addr());
		}
		// DEV / console diagnostics: RAC_PS4_SHOT=600,1800 saves those frames as pictures.
		if shot::wanted(s.flips as u64 + 1) {
			let (a, level) = s.textures[idx];
			shot::write(s.flips as u64 + 1, a.ptr, level.pitch, level.width, level.height);
		}
		s.flips += 1;
		plat::frame_done();
		let r = s.video.flip(idx, s.flips);
		if r != 0 {
			log::warn!("GNM: flip {idx} returned {r:#x}");
		}
		Ok(())
	}

	unsafe fn get_timestamp_period(&self) -> f32 {
		1.0
	}

	unsafe fn wait_for_idle(&self) -> Result<(), crate::DeviceError> {
		let last = *self.shared.last_submit.lock();
		if let Some((label, value)) = last {
			if !self.shared.wait_label(label, value, "the GPU (wait for idle)") {
				return Err(crate::DeviceError::Lost);
			}
		}
		Ok(())
	}
}

// ---------------------------------------------------------------------------------------------------------------------
// Device.

/// An image descriptor for a view of one level, `layers` layers (`addr` points at that level and first layer).
fn image_desc(addr: u64, fmt: &Fmt, level: &Level, layers: u32, dim: wgt::TextureViewDimension, depth_tiled: bool) -> [u32; 8] {
	image_desc_chain(addr, fmt, level, (0, 0), (0, layers), dim, depth_tiled, false)
}

/// An image descriptor in general: `addr` and `level` are what the texture unit counts from (a whole mip chain: the
/// texture's level 0), `mips` the (base, last) level and `arr` the (base layer, layer count) it may see; `pow2` sets
/// POW2_PAD, which tells it levels 1+ follow the power-of-two padded chain layout.
#[allow(clippy::too_many_arguments)]
fn image_desc_chain(addr: u64, fmt: &Fmt, level: &Level, mips: (u32, u32), arr: (u32, u32), dim: wgt::TextureViewDimension, depth_tiled: bool, pow2: bool) -> [u32; 8] {
	let (base_array, layers) = arr;
	let ty = match dim {
		wgt::TextureViewDimension::D1 => 8,
		wgt::TextureViewDimension::D2 => 9,
		wgt::TextureViewDimension::D3 => 10,
		wgt::TextureViewDimension::Cube => 11,
		wgt::TextureViewDimension::D2Array | wgt::TextureViewDimension::CubeArray => 13,
	};
	let tiling = if depth_tiled { desc::TILE_DEPTH_1D_THIN } else { desc::TILE_DISPLAY_LINEAR_ALIGNED };
	let a = addr >> 8;
	let [x, y, z, w] = fmt.dst_sel;
	let (depth_field, base_array, last_array) = match dim {
		wgt::TextureViewDimension::D3 => (layers.max(1) - 1, 0, 0),
		_ => (0, base_array, base_array + layers.max(1) - 1),
	};
	[
		a as u32,
		(a >> 32) as u32 & 0x3f | fmt.img_data << 20 | fmt.img_num << 26,
		(level.width.max(1) - 1) | (level.height.max(1) - 1) << 14,
		// BASE_LEVEL 15:12, LAST_LEVEL 19:16, TILING_INDEX 24:20, POW2_PAD 25, TYPE 31:28.
		x | y << 3 | z << 6 | w << 9 | (mips.0 & 15) << 12 | (mips.1 & 15) << 16 | tiling << 20 | (pow2 as u32) << 25 | ty << 28,
		depth_field | (level.pitch - 1) << 13,
		base_array & 0x1fff | last_array << 13,
		0,
		0,
	]
}

fn compare(c: wgt::CompareFunction) -> u32 {
	use wgt::CompareFunction as C;
	match c {
		C::Never => 0,
		C::Less => 1,
		C::Equal => 2,
		C::LessEqual => 3,
		C::Greater => 4,
		C::NotEqual => 5,
		C::GreaterEqual => 6,
		C::Always => 7,
	}
}

/// A sampler descriptor (S#).
fn sampler_desc(d: &crate::SamplerDescriptor<'_>) -> [u32; 4] {
	use wgt::AddressMode as A;
	let wrap = |m: A| match m {
		A::Repeat => 0,
		A::MirrorRepeat => 1,
		A::ClampToEdge => 2,
		A::ClampToBorder => 6,
	};
	let filter = |f: wgt::FilterMode| match f {
		wgt::FilterMode::Nearest => 0,
		wgt::FilterMode::Linear => 1,
	};
	// Anisotropy: MAX_ANISO_RATIO 0..4 = 1, 2, 4, 8, 16 samples.
	let aniso = match d.anisotropy_clamp {
		0 | 1 => 0,
		2..=3 => 1,
		4..=7 => 2,
		8..=15 => 3,
		_ => 4,
	};
	let mag = filter(d.mag_filter) | if aniso > 0 { 2 } else { 0 };
	let min = filter(d.min_filter) | if aniso > 0 { 2 } else { 0 };
	// MIP_FILTER: 0 none, 1 point, 2 linear.
	let mip = match d.mipmap_filter {
		wgt::MipmapFilterMode::Nearest => 1,
		wgt::MipmapFilterMode::Linear => 2,
	};
	let lod = |v: f32| ((v.clamp(0.0, 15.0) * 256.0) as u32) & 0xfff;
	let depth_compare = d.compare.map_or(0, compare);
	// Border colours: 0 transparent black, 1 opaque black, 2 opaque white.
	let border = match d.border_color {
		Some(wgt::SamplerBorderColor::OpaqueBlack) => 1,
		Some(wgt::SamplerBorderColor::OpaqueWhite) => 2,
		_ => 0,
	};
	[
		wrap(d.address_modes[0]) | wrap(d.address_modes[1]) << 3 | wrap(d.address_modes[2]) << 6 | aniso << 9 | depth_compare << 12,
		lod(d.lod_clamp.start) | lod(d.lod_clamp.end) << 12,
		mag << 20 | min << 22 | mip << 26,
		border << 30,
	]
}

impl crate::Device for Device {
	type A = Api;

	unsafe fn create_buffer(&self, desc: &crate::BufferDescriptor) -> DeviceResult<Buffer> {
		if desc.size == 0 {
			return Ok(Buffer { alloc: None, size: 0 });
		}
		if desc.usage.contains(wgt::BufferUses::MAP_READ) {
			if let Some(a) = self.shared.readback_heap().and_then(|h| h.alloc(desc.size as usize, 256)) {
				return Ok(Buffer { alloc: Some(a), size: desc.size });
			}
		}
		let a = self.shared.alloc_main(desc.size as usize, 256)?;
		Ok(Buffer { alloc: Some(a), size: desc.size })
	}

	unsafe fn destroy_buffer(&self, buffer: Buffer) {
		if let Some(a) = buffer.alloc {
			match self.shared.readback.get().and_then(|h| h.as_ref()).filter(|h| h.owns(&a)) {
				Some(h) => h.free(a),
				None => self.shared.main.free(a),
			}
		}
	}

	unsafe fn add_raw_buffer(&self, _buffer: &Buffer) {}

	unsafe fn map_buffer(&self, buffer: &Buffer, range: crate::MemoryRange) -> DeviceResult<crate::BufferMapping> {
		let base = buffer.alloc.map_or(ptr::NonNull::dangling().as_ptr(), |a| a.ptr);
		// SAFETY: wgpu-core keeps the range inside the buffer.
		let p = unsafe { base.add(range.start as usize) };
		Ok(crate::BufferMapping { ptr: ptr::NonNull::new(p).unwrap_or(ptr::NonNull::dangling()), is_coherent: true })
	}

	unsafe fn unmap_buffer(&self, buffer: &Buffer) {}
	unsafe fn flush_mapped_ranges<I>(&self, buffer: &Buffer, ranges: I) {
		// Write-combined memory: make the CPU's writes visible before the GPU reads.
		#[cfg(target_arch = "x86_64")]
		// SAFETY: a plain fence.
		unsafe {
			core::arch::x86_64::_mm_sfence()
		};
	}
	unsafe fn invalidate_mapped_ranges<I>(&self, buffer: &Buffer, ranges: I) {}

	unsafe fn create_texture(&self, desc: &crate::TextureDescriptor) -> DeviceResult<Texture> {
		let fmt = format::of(desc.format).ok_or_else(|| {
			log::error!("GNM: texture format {:?} is not supported", desc.format);
			crate::DeviceError::Unexpected
		})?;
		if desc.sample_count > 1 {
			self.shared.once("msaa", || alloc::format!("multisampled textures are drawn as single-sampled ({:?})", desc.label));
		}
		let depth = fmt.depth.is_some();
		let (w, h) = (desc.size.width, desc.size.height);
		let layers = desc.size.depth_or_array_layers.max(1);
		// A mipmapped colour texture is laid out the hardware's way (see `format::mip_pitch`), so a view of several levels
		// can be one T# with POW2_PAD; every other texture keeps the plain per-level layout.
		let chain = desc.mip_level_count > 1 && !depth;
		if chain && desc.dimension == wgt::TextureDimension::D3 {
			self.shared.once("mips-3d", || String::from("mipmapped 3D textures: levels beyond the first are laid out without halving the depth"));
		}
		let mut levels = Vec::new();
		let mut off = 0u64;
		for l in 0..desc.mip_level_count.max(1) {
			let (lw, lh) = ((w >> l).max(1), (h >> l).max(1));
			let (pitch, rows) = if chain {
				(format::mip_pitch(w, fmt.bytes, l, lh.next_power_of_two()), lh.next_power_of_two())
			} else {
				(format::pitch_texels(lw, fmt.bytes, depth), if depth { lh.next_multiple_of(8) } else { lh })
			};
			// Layers of a chain level follow each other unpadded (addrlib's slice size); levels start on 256 bytes.
			let raw = pitch as u64 * rows as u64 * fmt.bytes as u64;
			let layer_bytes = if chain && layers > 1 { raw } else { raw.next_multiple_of(256) };
			let depth_plane = if fmt.depth.is_some_and(|(z, _)| z == 0) { 0 } else { layer_bytes * layers as u64 };
			let level_off = off;
			off += depth_plane;
			let (stencil_offset, stencil_layer_bytes) = if fmt.depth.is_some_and(|(_, s)| s) {
				let sb = (pitch as u64 * rows as u64).next_multiple_of(256);
				let so = off.next_multiple_of(256);
				off = so + sb * layers as u64;
				(so, sb)
			} else {
				(0, 0)
			};
			off = off.next_multiple_of(256);
			levels.push(Level { offset: level_off, pitch, width: lw, height: lh, layer_bytes, stencil_offset, stencil_layer_bytes });
		}
		let a = self.shared.alloc_main(off.max(256) as usize, 64 << 10)?;
		// DEV (wgputest's mip address probe): every 32-bit word of the texture's memory holds its own word index, so a
		// texel load returns where the texture unit read it from.
		let probe = chain && std::env::var("RAC_GNM_FILL_OFFSETS").is_ok_and(|v| v == "1");
		if probe {
			let words = a.len / 4;
			// SAFETY: the whole allocation is ours and CPU-mapped (unified memory).
			let p = unsafe { core::slice::from_raw_parts_mut(a.ptr as *mut u32, words) };
			for (i, w) in p.iter_mut().enumerate() {
				*w = i as u32;
			}
			#[cfg(target_arch = "x86_64")]
			// SAFETY: a plain fence (write-combined memory).
			unsafe {
				core::arch::x86_64::_mm_sfence()
			};
		}
		Ok(Texture {
			alloc: Some(a),
			addr: a.addr(),
			format: desc.format,
			fmt,
			dimension: desc.dimension,
			layers,
			levels,
			chain,
			probe,
			surface_index: None,
		})
	}

	unsafe fn destroy_texture(&self, texture: Texture) {
		if let Some(a) = texture.alloc {
			self.shared.main.free(a);
		}
	}

	unsafe fn add_raw_texture(&self, _texture: &Texture) {}

	unsafe fn create_texture_view(&self, texture: &Texture, desc: &crate::TextureViewDescriptor) -> DeviceResult<TextureView> {
		let level_idx = desc.range.base_mip_level.min(texture.levels.len() as u32 - 1);
		let mips = desc.range.mip_level_count.unwrap_or(texture.levels.len() as u32 - level_idx).max(1);
		if mips > 1 && !texture.chain && desc.usage.contains(wgt::TextureUses::RESOURCE) {
			self.shared.once("mips", || String::from("depth texture views see only their base mip level"));
		}
		let layer = desc.range.base_array_layer;
		let layers = desc.range.array_layer_count.unwrap_or(texture.layers - layer);
		let level = texture.levels[level_idx as usize];
		let fmt = format::of(desc.format).unwrap_or(texture.fmt);
		let (addr, stencil_addr) = if texture.dimension == wgt::TextureDimension::D3 {
			(texture.subresource(level_idx, 0), 0)
		} else {
			(texture.subresource(level_idx, layer), texture.stencil_subresource(level_idx, layer))
		};
		let stencil_only = desc.range.aspect == wgt::TextureAspect::StencilOnly;
		let image = if stencil_only {
			let sfmt = format::of(wgt::TextureFormat::Stencil8).unwrap();
			image_desc(stencil_addr, &sfmt, &level, layers, desc.dimension, texture.is_depth())
		} else if texture.chain && mips > 1 {
			// Several levels: one T# over the whole chain from level 0, limited to the view's levels and layers. (A view
			// of one level keeps pointing straight at that level, which does not depend on the hardware's mip layout.)
			let (base_layer, d3) = (layer, texture.dimension == wgt::TextureDimension::D3);
			let arr = if d3 { (0, layers) } else { (base_layer, layers) };
			let last = (level_idx + mips - 1).min(texture.levels.len() as u32 - 1);
			image_desc_chain(texture.addr, &fmt, &texture.levels[0], (level_idx, last), arr, desc.dimension, false, true)
		} else {
			image_desc(addr, &fmt, &level, layers, desc.dimension, texture.is_depth())
		};
		Ok(TextureView {
			addr,
			stencil_addr,
			format: desc.format,
			fmt,
			level,
			layers,
			image,
			depth_aspect_only: desc.range.aspect == wgt::TextureAspect::DepthOnly,
		})
	}

	unsafe fn destroy_texture_view(&self, view: TextureView) {}

	unsafe fn create_sampler(&self, desc: &crate::SamplerDescriptor) -> DeviceResult<Sampler> {
		Ok(Sampler { words: sampler_desc(desc) })
	}

	unsafe fn destroy_sampler(&self, sampler: Sampler) {}

	unsafe fn create_command_encoder(&self, desc: &crate::CommandEncoderDescriptor<Queue>) -> DeviceResult<CommandEncoder> {
		Ok(CommandEncoder::new(Arc::clone(&self.shared)))
	}

	unsafe fn create_bind_group_layout(&self, desc: &crate::BindGroupLayoutDescriptor) -> DeviceResult<BindGroupLayout> {
		let key = crate::ps4key::bind_group_layout(desc);
		let layout = self.shared.cache.set_layout(key).map(|l| Arc::new(pipe::remap_to_wgpu(&l, desc.entries)));
		if layout.is_none() && !desc.entries.is_empty() {
			log::warn!("GNM: bind group layout {key:016x} ({:?}) is used by no cached pipeline", desc.label);
		}
		Ok(BindGroupLayout { key, entries: desc.entries.into(), layout })
	}

	unsafe fn destroy_bind_group_layout(&self, bg_layout: BindGroupLayout) {}

	unsafe fn create_pipeline_layout(&self, desc: &crate::PipelineLayoutDescriptor<BindGroupLayout>) -> DeviceResult<PipelineLayout> {
		let keys: Vec<Option<u64>> = desc.bind_group_layouts.iter().map(|g| g.map(|g| g.key)).collect();
		let dynamic_counts = desc
			.bind_group_layouts
			.iter()
			.map(|g| g.and_then(|g| g.layout.as_ref()).map_or(0, |l| l.dynamic_count))
			.collect();
		Ok(PipelineLayout { key: crate::ps4key::pipeline_layout(&keys, desc.immediate_size), dynamic_counts, immediate_size: desc.immediate_size })
	}

	unsafe fn destroy_pipeline_layout(&self, pipeline_layout: PipelineLayout) {}

	unsafe fn create_bind_group(
		&self,
		desc: &crate::BindGroupDescriptor<BindGroupLayout, Buffer, Sampler, TextureView, Resource>,
	) -> DeviceResult<BindGroup> {
		let Some(layout) = desc.layout.layout.as_ref() else {
			return Ok(BindGroup { mem: None, dynamic: Vec::new(), bufs: Vec::new(), shared: Arc::clone(&self.shared) });
		};
		let mem = if layout.size > 0 {
			let a = self.shared.alloc_low(layout.size as usize, 64)?;
			// SAFETY: a fresh allocation.
			unsafe { a.words().fill(0) };
			Some(a)
		} else {
			None
		};
		let mut dynamic = vec![(0u64, 0u32); layout.dynamic_count as usize];
		let mut bufs = Vec::new();
		for entry in desc.entries {
			let Some(slot) = layout.slots.get(&entry.binding).copied() else { continue };
			let Some(le) = desc.layout.entries.iter().find(|e| e.binding == entry.binding) else { continue };
			let words: Vec<u32> = match le.ty {
				wgt::BindingType::Buffer { .. } => {
					let b = &desc.buffers[entry.resource_index as usize];
					let size = b.size.map_or(b.buffer.size.saturating_sub(b.offset), |s| s.get());
					let addr = b.buffer.addr() + b.offset;
					if let pipe::Slot::Dynamic(n) = slot {
						dynamic[n as usize] = (addr, size as u32);
						continue;
					}
					bufs.push((entry.binding, addr, size));
					desc::buffer(addr, 0, size as u32).to_vec()
				}
				wgt::BindingType::Sampler(_) => desc.samplers[entry.resource_index as usize].words.to_vec(),
				wgt::BindingType::Texture { .. } | wgt::BindingType::StorageTexture { .. } => {
					desc.textures[entry.resource_index as usize].view.image.to_vec()
				}
				_ => {
					self.shared.once("binding-type", || alloc::format!("binding type {:?} is not supported", le.ty));
					continue;
				}
			};
			if let (pipe::Slot::Offset(off), Some(a)) = (slot, mem) {
				let off = off as usize / 4;
				// SAFETY: inside the set (radv's offsets), our allocation.
				let set = unsafe { a.words() };
				let n = words.len().min(set.len().saturating_sub(off));
				set[off..off + n].copy_from_slice(&words[..n]);
			}
		}
		Ok(BindGroup { mem, dynamic, bufs, shared: Arc::clone(&self.shared) })
	}

	unsafe fn destroy_bind_group(&self, group: BindGroup) {}

	unsafe fn create_shader_module(&self, desc: &crate::ShaderModuleDescriptor, shader: crate::ShaderInput) -> Result<ShaderModule, crate::ShaderError> {
		match shader {
			crate::ShaderInput::Naga(n) => Ok(ShaderModule { naga: Some(n) }),
			_ => Err(crate::ShaderError::Compilation(String::from("the GNM back end takes naga modules only"))),
		}
	}

	unsafe fn destroy_shader_module(&self, module: ShaderModule) {}

	unsafe fn create_render_pipeline(
		&self,
		desc: &crate::RenderPipelineDescriptor<PipelineLayout, ShaderModule, Resource>,
	) -> Result<RenderPipeline, crate::PipelineError> {
		Ok(RenderPipeline { inner: command::Gfx::new(&self.shared, desc).map(Arc::new) })
	}

	unsafe fn destroy_render_pipeline(&self, pipeline: RenderPipeline) {
		if let Some(g) = pipeline.inner.and_then(|g| Arc::try_unwrap(g).ok()) {
			g.free(&self.shared);
		}
	}

	unsafe fn create_compute_pipeline(
		&self,
		desc: &crate::ComputePipelineDescriptor<PipelineLayout, ShaderModule, Resource>,
	) -> Result<ComputePipeline, crate::PipelineError> {
		Ok(ComputePipeline { inner: command::Comp::new(&self.shared, desc).map(Arc::new) })
	}

	unsafe fn destroy_compute_pipeline(&self, pipeline: ComputePipeline) {
		if let Some(c) = pipeline.inner.and_then(|c| Arc::try_unwrap(c).ok()) {
			c.free(&self.shared);
		}
	}

	unsafe fn create_ray_tracing_pipeline(
		&self,
		desc: &crate::RayTracingPipelineDescriptor<PipelineLayout, ShaderModule, Resource>,
	) -> Result<Resource, crate::PipelineError> {
		Err(crate::PipelineError::Linkage(wgt::ShaderStages::empty(), String::from("no ray tracing on the PS4")))
	}

	unsafe fn destroy_ray_tracing_pipeline(&self, pipeline: Resource) {}

	unsafe fn get_raytracing_pipeline_group_data(&self, pipeline: &Resource, groups: core::ops::Range<u32>) -> Result<Vec<u8>, crate::DeviceError> {
		Ok(Vec::new())
	}

	unsafe fn create_pipeline_cache(&self, desc: &crate::PipelineCacheDescriptor<'_>) -> Result<Resource, crate::PipelineCacheError> {
		Ok(Resource)
	}

	unsafe fn destroy_pipeline_cache(&self, cache: Resource) {}

	unsafe fn create_query_set(&self, desc: &wgt::QuerySetDescriptor<crate::Label>) -> DeviceResult<Resource> {
		self.shared.once("queries", || String::from("query sets are not supported (they stay empty)"));
		Ok(Resource)
	}

	unsafe fn destroy_query_set(&self, set: Resource) {}

	unsafe fn create_fence(&self) -> DeviceResult<Fence> {
		// Never freed (see `Fence`).
		let a = self.shared.alloc_low(8, 8)?;
		let label = a.ptr as *const AtomicU64;
		// SAFETY: a fresh, 8-byte aligned allocation.
		unsafe { (*label).store(0, Ordering::Release) };
		Ok(Fence { label })
	}

	unsafe fn destroy_fence(&self, fence: Fence) {}

	unsafe fn get_fence_value(&self, fence: &Fence) -> DeviceResult<crate::FenceValue> {
		Ok(fence.value())
	}

	unsafe fn wait(&self, fence: &Fence, value: crate::FenceValue, timeout: Option<Duration>) -> DeviceResult<bool> {
		let t = std::time::Instant::now();
		while fence.value() < value {
			if timeout.is_some_and(|d| t.elapsed() >= d) {
				return Ok(false);
			}
			std::thread::yield_now();
		}
		Ok(true)
	}

	unsafe fn start_graphics_debugger_capture(&self) -> bool {
		false
	}

	unsafe fn stop_graphics_debugger_capture(&self) {}

	unsafe fn create_acceleration_structure(&self, desc: &crate::AccelerationStructureDescriptor) -> DeviceResult<Resource> {
		Err(crate::DeviceError::Unexpected)
	}

	unsafe fn get_acceleration_structure_build_sizes<'a>(
		&self,
		_desc: &crate::GetAccelerationStructureBuildSizesDescriptor<'a, Buffer>,
	) -> crate::AccelerationStructureBuildSizes {
		Default::default()
	}

	unsafe fn get_acceleration_structure_device_address(&self, _acceleration_structure: &Resource) -> wgt::BufferAddress {
		0
	}

	unsafe fn destroy_acceleration_structure(&self, _acceleration_structure: Resource) {}

	fn tlas_instance_to_bytes(&self, instance: TlasInstance) -> Vec<u8> {
		Vec::new()
	}

	fn get_internal_counters(&self) -> wgt::HalCounters {
		Default::default()
	}

	fn check_if_oom(&self) -> DeviceResult<()> {
		Ok(())
	}
}
