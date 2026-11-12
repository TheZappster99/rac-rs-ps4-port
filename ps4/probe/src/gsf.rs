//! The game's own picture on the PS4: rac-rs's GS-full pipeline (rac-rs/src/gsfull, compiled into the probe unchanged) runs the
//! VIF1 stream, the VU1 microprograms and a software GS on the CPU, so no PS4 GPU API is needed for a first picture. The PC
//! renderer is wgpu (Vulkan), which the console does not have.
//!
//! Like rac-rs `gsfull::pipe::start`, but with thread stacks that fit the PS4's ~280 MB of flexible memory (the PC's are 64 MB
//! each) and a frame slot the presenter (vout.rs) shares:
//!
//!   EE thread (main.rs) --FieldIn--> "gsfull-vu1": VIF1 + VU1 --Field--> "gsfull-gs": raster + CRTC read -> slot -> vout

use crate::gsfull::crtc::{present, Crtc};
use crate::gsfull::gs::Gs;
use crate::gsfull::pipe::{raster, Field, FieldIn, Pipe};
use ee_oracle::gs::Job;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One finished picture of the game: RGBA8, alpha 255.
pub struct Frame {
	pub field: u64,
	pub w: u32,
	pub h: u32,
	pub rgba: Vec<u8>,
}

/// What the stages share with the EE thread and the presenter.
#[derive(Default)]
pub struct Shared {
	/// The newest picture (the presenter takes it).
	pub slot: Mutex<Option<Frame>>,
	/// Work time of each stage, ns, and how many fields each ran, since the last `report`.
	vu_ns: AtomicU64,
	vu_n: AtomicU64,
	vu_max: AtomicU64,
	ras_ns: AtomicU64,
	ras_n: AtomicU64,
	/// Time the EE thread spent waiting for the first stage (the pipeline is slower than the game), ns.
	ee_wait_ns: AtomicU64,
	/// Pictures drawn in total.
	pub pictures: AtomicU64,
	/// A stage ended (panic or closed channel).
	pub dead: AtomicBool,
}

pub struct Gsf {
	tx: SyncSender<FieldIn>,
	jobs: Receiver<Job>,
	pub shared: Arc<Shared>,
}

struct Flag(Arc<Shared>);
impl Drop for Flag {
	fn drop(&mut self) {
		self.0.dead.store(true, Ordering::SeqCst);
	}
}

/// Stack of the VIF1 + VU1 stage and of the raster stage. The PC gives each 64 MB (rac-rs pipe.rs); the PS4 counts thread stacks
/// against its small flexible memory budget (208 MB were left once the 32 MB game thread was up).
const VU_STACK_MB: usize = 32;
const GS_STACK_MB: usize = 16;

/// A picture of every this many fields is written to `snap_dir` as frame_NNNNN.ppm.
const SNAP_EVERY: u64 = 1000;

/// Starts the pipeline and points the machine's graphics jobs at it. `threads` = rasteriser workers (rac-rs `gsfull::pipe::threads`).
/// `draw` false: VIF1 + VU1 only, each field dropped before the software GS (the VU1 stage's cost with the game at its own speed).
pub fn start(m: &mut ee_oracle::Machine, threads: usize, snap_dir: &str, draw: bool) -> Option<Gsf> {
	let snap_dir = snap_dir.to_string();
	// A cold boot: blank local memory and VU1, as the console's after the BIOS; the boot uploads its own VU1 programs.
	let vram = vec![0u8; crate::gsfull::gs_mem::VRAM_SIZE];
	let mut pipe = Pipe::new("")?;
	let shared = Arc::new(Shared::default());
	let (tx, rx) = sync_channel::<FieldIn>(1);
	let (ftx, frx) = sync_channel::<Field>(1);
	let sh = shared.clone();
	// PROBE_CENSUS=FROM:TO (PC): a census of the GS work of those fields (census.rs) into census.txt next to the snapshots.
	let census_range: Option<(u64, u64)> = std::env::var("PROBE_CENSUS").ok().and_then(|v| {
		let (a, b) = v.split_once(':')?;
		Some((a.parse().ok()?, b.parse().ok()?))
	});
	let census_path = format!("{snap_dir}/census.txt");
	std::thread::Builder::new()
		.name("gsfull-vu1".into())
		.stack_size(VU_STACK_MB << 20)
		.spawn(move || {
			let _flag = Flag(sh.clone());
			let mut census = crate::census::Census::default();
			let mut n = 0u64;
			while let Ok(inp) = rx.recv() {
				let field = n;
				n += 1;
				if inp.jobs.is_empty() {
					continue;
				}
				let t = Instant::now();
				let mut f = pipe.process(inp.jobs, inp.crtc);
				f.field = field;
				if let Some((from, to)) = census_range {
					if (from..to).contains(&field) {
						census.field(&f.qs, &f.segs);
						if field + 1 == to {
							let _ = std::fs::write(&census_path, census.report());
						}
					}
				}
				let ns = t.elapsed().as_nanos() as u64;
				sh.vu_ns.fetch_add(ns, Ordering::Relaxed);
				sh.vu_max.fetch_max(ns, Ordering::Relaxed);
				sh.vu_n.fetch_add(1, Ordering::Relaxed);
				if !draw {
					continue;
				}
				if ftx.send(f).is_err() {
					break;
				}
			}
		})
		.ok()?;
	let sh = shared.clone();
	std::thread::Builder::new()
		.name("gsfull-gs".into())
		.stack_size(GS_STACK_MB << 20)
		.spawn(move || {
			let _flag = Flag(sh.clone());
			let mut g = Gs::new(vram, threads);
			while let Ok(f) = frx.recv() {
				let t = Instant::now();
				raster(&mut g, &f);
				let pic = present(&mut g, &f.crtc);
				sh.ras_ns.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
				sh.ras_n.fetch_add(1, Ordering::Relaxed);
				sh.pictures.fetch_add(1, Ordering::Relaxed);
				let fr = Frame { field: f.field, w: pic.w, h: pic.h, rgba: pic.rgba };
				if (f.field + 1) % SNAP_EVERY == 0 {
					let _ = write_ppm(&format!("{snap_dir}/frame_{:05}.ppm", f.field + 1), &fr);
				}
				if let Ok(mut s) = sh.slot.lock() {
					*s = Some(fr);
				}
			}
		})
		.ok()?;
	let (jtx, jrx) = std::sync::mpsc::channel::<Job>();
	m.cpu.mem.gsq = Some(jtx);
	Some(Gsf { tx, jobs: jrx, shared })
}

impl Gsf {
	/// After each `Machine::frame`: the field's graphics jobs and the CRTC registers go down the pipeline (as rac-rs
	/// retail/worker/field_gs.rs). Blocks while the pipeline is a field behind, so a slow picture slows the game rather than
	/// dropping jobs (they carry state such as texture uploads).
	pub fn field(&self, m: &ee_oracle::Machine) {
		let inp = FieldIn { jobs: self.jobs.try_iter().collect(), crtc: Crtc::from_regs(&m.cpu.mem.gs) };
		let t = Instant::now();
		let _ = self.tx.send(inp);
		self.shared.ee_wait_ns.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
	}

	/// Mean ms per field of each stage since the last call, and the game thread's wait per field over `fields` fields.
	pub fn report(&self, fields: u64) -> String {
		let s = &self.shared;
		let take = |a: &AtomicU64| a.swap(0, Ordering::Relaxed);
		let (vn, vt, rn, rt, ew, vmax) = (take(&s.vu_n), take(&s.vu_ns), take(&s.ras_n), take(&s.ras_ns), take(&s.ee_wait_ns), take(&s.vu_max));
		let ms = |t: u64, n: u64| t as f64 / 1e6 / n.max(1) as f64;
		format!(
			"GS-full: vif+vu1 {:.1} ms (x{vn}, worst {:.1}), raster+read {:.1} ms (x{rn}), game waited {:.1} ms a field, {} pictures{}",
			ms(vt, vn),
			vmax as f64 / 1e6,
			ms(rt, rn),
			ms(ew, fields),
			s.pictures.load(Ordering::Relaxed),
			if s.dead.load(Ordering::SeqCst) { ", A STAGE STOPPED" } else { "" }
		)
	}
}

/// Writes a picture as a binary PPM (a quick look at what the pipeline drew, on the PC or fetched from the console by FTP).
pub fn write_ppm(path: &str, f: &Frame) -> std::io::Result<()> {
	let mut b = format!("P6\n{} {}\n255\n", f.w, f.h).into_bytes();
	b.reserve((f.w * f.h * 3) as usize);
	for p in f.rgba.chunks_exact(4) {
		b.extend_from_slice(&p[..3]);
	}
	std::fs::write(path, b)
}
