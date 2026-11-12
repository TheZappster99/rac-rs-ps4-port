//! PS4 port probe: cold-boots the game from the disc image with only the core (no picture, sound or pad), presses Cross and
//! Start now and then so the logos and title screen move on, and logs every few seconds how many fields ran, the level and the
//! speed against a real PS2. The answer to "does the core run on the console, and fast enough?".
//!
//!   rac-ps4-probe [ISO] [STUBS] [LOG] [FIELDS]
//!
//! On the PS4 there are no arguments: everything lives in /data/rac-rs (the console's writable data folder, reachable by FTP).

use std::io::Write;
use std::time::Instant;

#[cfg(target_os = "windows")]
const DIR: &str = ".";
#[cfg(not(target_os = "windows"))]
const DIR: &str = "/data/rac-rs";

// The C functions the PS4 lacks (ps4/shim).
use rac_ps4_shim as _;

#[cfg(target_os = "freebsd")]
mod prof;
#[cfg(target_os = "windows")]
#[path = "prof_win.rs"]
mod prof;
// The picture: rac-rs's GS-full pipeline compiled in unchanged (gsf.rs), shown on the TV (vout.rs).
#[path = "../../../rac-rs/src/engine/gsfull/mod.rs"]
#[allow(dead_code, unused_imports, unused_variables, unused_mut, unused_parens, unsafe_op_in_unsafe_fn, clippy::all)]
mod gsfull;
mod census;
mod gsf;
mod standins;
mod vout;
use standins::{gscap, level, render, retail, texpack};

/// The fields the profiler samples (gameplay, after the default script loaded the save).
const PROF_FROM: u64 = 1600;
const PROF_TO: u64 = 2800;


// The retail movie player (one copy per level overlay and one in the front end) cannot run here: the oracle has no IPU, so
// it spins, and that spin grew the game thread's stack until it overflowed (PC: 256 MB after ~1400 fields). The PC game
// replaces it with a native player (rac-rs retail/fmv_bridge.rs); headless, every movie is skipped at once, as the PC game
// does without a window. (entry, end, hash) as in fmv_bridge::PLAYERS / disc::PLAYERS_USA; the boot copy is already
// skipped by the cold boot's own hook rule.
const MOVIES_PAL: [(u32, u32, u64); 20] = [
	(0x2ed6d0, 0x2ed778, 0x5ba1_5322_13d4_d30c), (0x319b90, 0x319c38, 0xc7ad_5d0c_cc71_a6dc), (0x2fc2b8, 0x2fc360, 0x94d8_295b_3a28_6e46),
	(0x2ed3c8, 0x2ed470, 0x0ded_5c58_aad6_94ca), (0x2f13e8, 0x2f1490, 0x965c_f8c5_a4b0_3b52), (0x32b770, 0x32b818, 0xdc30_bf36_eff2_0e94),
	(0x316318, 0x3163c0, 0xdfa6_8f5f_a0e2_da9e), (0x329300, 0x3293a8, 0xdf6f_8f94_fa91_bf78), (0x316c98, 0x316d40, 0x9bd3_909b_99dd_aa3e),
	(0x315710, 0x3157b8, 0x64eb_67f2_8fd5_85fc), (0x2f68e0, 0x2f6988, 0xf3a6_ec18_e419_72d0), (0x3288d0, 0x328978, 0xaaab_f7cb_c872_13cc),
	(0x316698, 0x316740, 0x8fce_7544_7a18_9bbe), (0x318028, 0x3180d0, 0xe050_f1fb_8964_4dc2), (0x316c00, 0x316ca8, 0xf086_9706_2d70_c978),
	(0x2f8d70, 0x2f8e18, 0x693e_1b8b_9baa_1a54), (0x2f57e8, 0x2f5890, 0x7e7c_b450_3739_da12), (0x300848, 0x3008f0, 0x94de_2136_8b35_4d2a),
	(0x306a48, 0x306af0, 0x76a2_0dd7_51c4_0faa), (0x239880, 0x239928, 0x167e_6217_a13a_cbd8),
];
const MOVIES_USA: [(u32, u32, u64); 20] = [
	(0x2edd28, 0x2eddd0, 0x9c1c_6907_3511_5802), (0x31a260, 0x31a308, 0xd6a4_14ca_fc97_5bb0), (0x2fc988, 0x2fca30, 0x28e4_f49b_05b5_ec5a),
	(0x2eda98, 0x2edb40, 0xbcea_4992_0adf_213e), (0x2f1ac0, 0x2f1b68, 0xae83_6498_412f_4b88), (0x32bdb0, 0x32be58, 0x09fe_00c7_4821_0404),
	(0x3169f0, 0x316a98, 0x62d8_f375_e849_6eb4), (0x3299d8, 0x329a80, 0xdf61_f79c_fd12_a1ae), (0x317368, 0x317410, 0x172d_251e_2876_fff2),
	(0x315de8, 0x315e90, 0xa5fb_39d3_7a81_5652), (0x2f6fb8, 0x2f7060, 0x8a92_80de_6590_c386), (0x328fa0, 0x329048, 0x2fcb_e50c_9dd6_b760),
	(0x316d68, 0x316e10, 0xbc6a_0e57_b632_1172), (0x318700, 0x3187a8, 0xc076_2877_7335_2bf8), (0x317250, 0x3172f8, 0x8cc4_381a_8b9d_7d6c),
	(0x2f9440, 0x2f94e8, 0xa130_c48d_4abc_51e8), (0x2f5ed8, 0x2f5f80, 0xe3d1_09f9_c041_536e), (0x300f20, 0x300fc8, 0x6171_b2e6_f4c2_9bc0),
	(0x307118, 0x3071c0, 0xd8cc_1537_ce5a_559e), (0x239d70, 0x239e18, 0x356d_e756_ae3b_5214),
];

static SKIPPED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The movie is over at once: v0 = 0, retail's return value.
fn skip_movie(c: &mut ee_oracle::cpu::Cpu) -> bool {
	SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	c.s(2, 0);
	true
}

const fn hooks(t: &[(u32, u32, u64); 20]) -> [ee_oracle::rec::Hand; 20] {
	const NONE: ee_oracle::rec::Hand = ee_oracle::rec::Hand { entry: 0, end: 0, hash: 0, est: 0, f: skip_movie };
	let mut v = [NONE; 20];
	let mut i = 0;
	while i < 20 {
		v[i] = ee_oracle::rec::Hand { entry: t[i].0, end: t[i].1, hash: t[i].2, est: 0, f: skip_movie };
		i += 1;
	}
	v
}
static HOOKS_PAL: [ee_oracle::rec::Hand; 20] = hooks(&MOVIES_PAL);
static HOOKS_USA: [ee_oracle::rec::Hand; 20] = hooks(&MOVIES_USA);

// The PS4's memory counters (libkernel): "flexible" memory is what malloc, mmap and thread stacks draw from, a small budget
// per app; "direct" memory is the large pool games map themselves.
#[cfg(target_os = "freebsd")]
unsafe extern "C" {
	fn sceKernelAvailableFlexibleMemorySize(size: *mut usize) -> i32;
	fn sceKernelGetDirectMemorySize() -> usize;
}

fn mem_line() -> String {
	#[cfg(target_os = "freebsd")]
	{
		let mut flex = 0usize;
		// SAFETY: plain out-parameter calls into libkernel.
		let r = unsafe { sceKernelAvailableFlexibleMemorySize(&mut flex) };
		let direct = unsafe { sceKernelGetDirectMemorySize() };
		format!("flexible memory free {} MB (r={r:#x}), direct memory {} MB", flex >> 20, direct >> 20)
	}
	#[cfg(not(target_os = "freebsd"))]
	String::from("memory counters are PS4 only")
}

fn main() {
	// The unwinder must know where the unwind tables are before anything can panic (ps4/shim).
	#[cfg(target_os = "freebsd")]
	rac_ps4_shim::init();
	let a: Vec<String> = std::env::args().skip(1).collect();
	let iso = a.first().cloned().unwrap_or_else(|| format!("{DIR}/game.iso"));
	// The PKG carries stubs.txt in /app0 (read-only, next to the eboot); on the PC it is an argument.
	let stubs = a.get(1).cloned().unwrap_or_else(|| if cfg!(target_os = "windows") { format!("{DIR}/stubs.txt") } else { "/app0/stubs.txt".into() });
	let log_path = a.get(2).cloned().unwrap_or_else(|| format!("{DIR}/probe.log"));
	let fields: u64 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(3000);

	let mut log = std::fs::File::create(&log_path).ok();
	let mut say = |s: String| {
		println!("{s}");
		if let Some(f) = log.as_mut() {
			let _ = writeln!(f, "{s}");
			let _ = f.flush();
		}
	};
	say(format!("probe: iso {iso}, stubs {stubs}, {fields} fields"));
	// A panic anywhere (also on the game thread) lands in the log before the process goes.
	let lp = log_path.clone();
	std::panic::set_hook(Box::new(move |p| {
		if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&lp) {
			let _ = writeln!(f, "probe: PANIC {p}");
		}
		eprintln!("probe: PANIC {p}");
	}));

	// The game thread's stack. The PC's 256 MB does not fit the PS4's memory budget for apps next to the 166 MB of game code
	// (thread creation failed with "resource temporarily unavailable"); 5th argument or ORACLE_EE_STACK_MB on the PC.
	let stack_mb = a.get(4).cloned().or_else(|| std::env::var("ORACLE_EE_STACK_MB").ok()).unwrap_or_else(|| "32".into());
	// SAFETY: still single-threaded here.
	unsafe { std::env::set_var("ORACLE_EE_STACK_MB", &stack_mb) };
	say(format!("probe: game thread stack {stack_mb} MB; {}", mem_line()));

	let t0 = Instant::now();
	let mut m = match ee_oracle::Machine::cold(&iso, &stubs) {
		Ok(m) => m,
		Err(e) => {
			say(format!("probe: cold boot failed: {e}"));
			return;
		}
	};
	ee_game::register();
	let usa = ee_oracle::ver::disc() == ee_oracle::ver::Disc::Usa;
	ee_oracle::rec::register_hand(if usa { &HOOKS_USA } else { &HOOKS_PAL });
	// The player's memory card (rac-rs player/card.bin, copied next to game.iso), so the script can load a save and the run
	// measures gameplay, not only the menus. As rac-rs retail/memcard.rs `restore_bytes`.
	let card_path = std::env::var("PROBE_CARD").unwrap_or_else(|_| format!("{DIR}/card.bin"));
	match std::fs::read(&card_path) {
		Ok(b) if b.len() >= 8 => {
			let mut mc = m.cpu.mc.clone();
			let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
				mc.load_dyn(&mut ee_oracle::snapfile::Rd::new(&b));
				mc
			}));
			match ok {
				Ok(mut mc) => {
					mc.fds = (0..3).map(|_| None).collect();
					mc.list.clear();
					mc.list_pos = 0;
					mc.bump();
					m.cpu.mc = mc;
					say(format!("probe: memory card loaded from {card_path}"));
				}
				Err(_) => say(format!("probe: memory card {card_path} unreadable")),
			}
		}
		_ => say(format!("probe: no memory card at {card_path} (menus only)")),
	}
	let script = script();
	let n = m.enable_rec();
	say(format!("probe: machine ready in {:.2}s, {n} recompiled functions", t0.elapsed().as_secs_f64()));
	say(format!("probe: {}", mem_line()));
	// One field of a real PS2 in the disc's video mode (PAL 50, NTSC 59.94 a second).
	let field_secs = 1.0 / ee_oracle::ver::field_hz();
	say(format!("probe: {} disc, {:.2} fields a second", if usa { "USA" } else { "PAL" }, ee_oracle::ver::field_hz()));

	unwind_bench(&mut say);

	// The picture: GS-full gets every graphics job from the first field on, the presenter shows what it draws.
	let gs_threads = gs_threads();
	// PROBE_GS (PC) or gs_mode.txt next to the log: `full` draws the picture; `vu` (the default of this build, step 2 part 4's
	// first measurement) runs VIF1 + VU1 and drops each field before the software GS, so the game runs at its own speed and the
	// log shows what the VU1 stage costs in a level. The TV keeps the colour bars then.
	let mode = std::env::var("PROBE_GS").ok().or_else(|| std::fs::read_to_string(format!("{DIR}/gs_mode.txt")).ok()).map(|s| s.trim().to_string()).unwrap_or_else(|| "vu".into());
	let draw = mode == "full";
	let gsf = gsf::start(&mut m, gs_threads, DIR, draw);
	match &gsf {
		Some(g) => {
			say(format!("probe: GS-full {} ({gs_threads} raster threads); {}", if draw { "picture on" } else { "VIF1 + VU1 only, no picture (gs_mode.txt: full for the picture)" }, mem_line()));
			vout::start(g.shared.clone(), &mut say);
		}
		None => say("probe: GS-full could not start (no picture)".into()),
	}

	// Per report window: field time split into fields that unwound the stack (the field after the flag is set: the game
	// thread unwinds when it wakes up at the vblank wait) and all others.
	let (mut t_norm, mut n_norm, mut t_unw, mut n_unw, mut max_unw) = (0f64, 0u32, 0f64, 0u32, 0f64);
	let mut unwinds_now = false;
	let every = 10u64;
	let start = Instant::now();
	let mut game_start = Instant::now();
	let mut mark = Instant::now();
	let mut mark_field = 0u64;
	let mut level = m.level();
	// The front end's mode word (rac-rs title_boot.rs FRONT_MODE: 0 title idle, 3 page menu, 4 text screen), logged when it changes
	// so a pad script can be checked against the menus it passes.
	let front_word = if usa { 0x15f604 } else { 0x15f5e4 };
	let mut front = u32::MAX;
	for f in 0..fields {
		// Cross for 3 fields every 2 s and Start every 5 s: skips the logo movies and the title screen, then idles in menus.
		let b = pad_at(&script, f);
		// In the level: walk (left stick forward) 3 s of every 5, jump now and then, so the run is gameplay rather than idling.
		let walk = f >= WALK_FROM && (f - WALK_FROM) % 300 < 180;
		let b = if f >= WALK_FROM && (f - WALK_FROM) % 90 < 4 { b | 0x40 } else { b };
		m.set_pad(b, if walk { [0x80, 0x80, 0x80, 0x00] } else { [0x80; 4] });
		if f == WALK_FROM {
			game_start = Instant::now();
		}
		let tf = Instant::now();
		let r = m.frame();
		let ms = tf.elapsed().as_secs_f64() * 1000.0;
		if unwinds_now {
			t_unw += ms;
			n_unw += 1;
			max_unw = max_unw.max(ms);
		} else {
			t_norm += ms;
			n_norm += 1;
		}
		unwinds_now = false;
		if let Err(e) = r {
			say(format!("probe: field {f}: stopped: {e}"));
			break;
		}
		if let Some(g) = &gsf {
			g.field(&m);
		}
		if f == 0 {
			say(format!("probe: first field ran (game thread up); {}", mem_line()));
			// The game thread exists now (with SIGPROF open); the profiler samples it during the menus.
			prof::start(&mut say);
		}
		if f + 1 == PROF_FROM {
			prof::set(true);
			say(format!("probe: field {}: profiling the game thread", f + 1));
		}
		if f + 1 == PROF_TO {
			prof::write(&format!("{DIR}/profile.txt"), &mut say);
		}
		// The compiled code never returns from the game's endless loops, so its native frames pile up (about 160 KB a field, a
		// 256 MB stack overflowed after ~1400 fields). The PC game unwinds them every 10 fields (rac-rs retail/worker.rs
		// UNWIND_FIELDS) while the game thread is parked at the vblank wait; so does this.
		if (f + 1) % every == 0 {
			m.cpu.rec_unwind = true;
			unwinds_now = true;
		}
		if std::env::var_os("PROBE_FRONT").is_some() && m.rd32(front_word) != front {
			front = m.rd32(front_word);
			say(format!("probe: field {f}: front mode {front}, pad {b:#x}"));
		}
		if m.level() != level {
			level = m.level();
			say(format!("probe: field {f}: level {level}"));
		}
		if mark.elapsed().as_secs_f64() >= 5.0 || f + 1 == fields {
			let ran = f + 1 - mark_field;
			let secs = mark.elapsed().as_secs_f64();
			say(format!(
				"probe: field {}: level {level}, {:.1} fields/s ({:.0}% of PS2 speed)",
				f + 1,
				ran as f64 / secs,
				100.0 * ran as f64 * field_secs / secs
			));
			say(format!(
				"probe:   field time {:.1} ms (x{n_norm}), unwinding fields {:.1} ms (x{n_unw}, worst {:.1} ms)",
				t_norm / n_norm.max(1) as f64,
				t_unw / n_unw.max(1) as f64,
				max_unw
			));
			if let Some(g) = &gsf {
				say(format!("probe:   {}", g.report(ran)));
			}
			(t_norm, n_norm, t_unw, n_unw, max_unw) = (0.0, 0, 0.0, 0, 0.0);
			mark = Instant::now();
			mark_field = f + 1;
		}
	}
	say(format!("probe: {} movies skipped", SKIPPED.load(std::sync::atomic::Ordering::Relaxed)));
	let secs = start.elapsed().as_secs_f64();
	if fields > WALK_FROM {
		let g = game_start.elapsed().as_secs_f64();
		say(format!("probe: in the level (fields {WALK_FROM}..{fields}, level {}): {:.0}% of PS2 speed", m.level(), 100.0 * (fields - WALK_FROM) as f64 * field_secs / g));
	}
	say(format!("probe: done, {:.1}s, average {:.0}% of PS2 speed", secs, 100.0 * fields as f64 * field_secs / secs));
	// Returning from main crashes on the console (CE-34878-0); quit the PS4 way (ps4/shim). The game thread is left parked.
	#[cfg(target_os = "freebsd")]
	rac_ps4_shim::quit(0);
}

/// What one unwind costs here, apart from the game: a caught `resume_unwind` through 1 and through 500 nested frames (the
/// game thread unwinds through thousands of compiled frames every 10 fields).
fn unwind_bench(say: &mut impl FnMut(String)) {
	#[inline(never)]
	fn deep(n: u32) -> u32 {
		if n == 0 {
			std::panic::resume_unwind(Box::new(0u8));
		}
		std::hint::black_box(deep(std::hint::black_box(n - 1))) + 1
	}
	for depth in [1u32, 500] {
		let t = Instant::now();
		let runs = 20;
		for _ in 0..runs {
			let _ = std::panic::catch_unwind(|| deep(depth));
		}
		say(format!("probe: one unwind through {depth} frames: {:.3} ms", t.elapsed().as_secs_f64() * 1000.0 / runs as f64));
	}
}

/// The scripted pad: (first field, buttons) taps, each held for 4 fields. From PROBE_SCRIPT (PC) or `script.txt` next to the
/// log, else the built-in one: through the logos and title screen to Load Game and the first save. Format: `FIELD:BUTTONS`
/// separated by commas, BUTTONS joined by `+` from x o t s start select up down left right l1 l2 r1 r2.
fn script() -> Vec<(u64, u16)> {
	let text = std::env::var("PROBE_SCRIPT").ok().or_else(|| std::fs::read_to_string(format!("{DIR}/script.txt")).ok()).unwrap_or_else(|| DEFAULT_SCRIPT.into());
	text.split(',')
		.filter_map(|e| {
			let (f, b) = e.trim().split_once(':')?;
			let bits = b.split('+').map(|n| match n.trim() {
				"x" => 0x40,
				"o" => 0x20,
				"t" => 0x10,
				"s" => 0x80,
				"start" => 0x800,
				"select" => 0x100,
				"up" => 0x1000,
				"right" => 0x2000,
				"down" => 0x4000,
				"left" => 0x8000,
				"l2" => 0x1,
				"r2" => 0x2,
				"l1" => 0x4,
				"r1" => 0x8,
				_ => 0,
			});
			Some((f.trim().parse().ok()?, bits.fold(0u16, |a, b| a | b)))
		})
		.collect()
}

/// Checked on the PC with the USA disc: Start opens the title menu at field 220 (front mode 3), Down + Cross pick Load Game, Cross
/// loads the first save at ~430 and the level is playable from ~1160.
const DEFAULT_SCRIPT: &str = "220:start,280:down,320:x,420:x";
/// From here the level runs with the walk / jump pattern and counts as gameplay.
const WALK_FROM: u64 = 1300;

/// Rasteriser workers of the software GS: PROBE_GS_THREADS (PC) or `gs_threads.txt` next to the log, else 3 on the PS4 (its 6
/// usable cores also run the game, the VU1 stage, the raster stage and the presenter) and rac-rs's default on the PC.
fn gs_threads() -> usize {
	let set = std::env::var("PROBE_GS_THREADS").ok().or_else(|| std::fs::read_to_string(format!("{DIR}/gs_threads.txt")).ok());
	set.and_then(|s| s.trim().parse().ok()).unwrap_or(if cfg!(target_os = "freebsd") { 3 } else { gsfull::pipe::default_threads() })
}

fn pad_at(script: &[(u64, u16)], f: u64) -> u16 {
	script.iter().filter(|(s, _)| f >= *s && f < s + 4).fold(0, |a, (_, b)| a | b)
}
