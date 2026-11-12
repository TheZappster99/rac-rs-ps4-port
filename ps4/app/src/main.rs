// The game as the PC exe builds it (rac-rs/exe/src/main.rs), linked against the GNM back end of the vendored wgpu-hal.
// - PC: the back end is a fake GPU (it records, draws nothing): WGPU_BACKEND=noop WGPU_NOOP_BACKEND=1 puts the window on it. That is
//   the check that every pipeline the game makes is in the shader cache.
// - PS4 (target os freebsd): the real GPU. The console starts the eboot without arguments or environment, so `ps4_start` sets up
//   what Play.cmd sets on the PC (disc, stubs, hand-skip list, player folder) with everything in /data/rac-rs, sends stdout / stderr
//   into /data/rac-rs/log.txt, and then the game's own `main` runs. The viewer's window loop is viewer/ps4win.rs there.

// The console profiler notes every thread through std's spawn hook (build-std runs with RUSTC_BOOTSTRAP=1).
#![cfg_attr(target_os = "freebsd", feature(thread_spawn_hook))]

use std::process::ExitCode;

// The C functions the PS4 lacks, the unwinder and `quit` (ps4/shim).
use rac_ps4_shim as _;

#[cfg(windows)]
#[path = "prof_win.rs"]
mod prof;
#[cfg(target_os = "freebsd")]
mod prof;

#[cfg(target_os = "freebsd")]
mod heap;
#[cfg(target_os = "freebsd")]
#[global_allocator]
static HEAP: heap::Heap = heap::Heap::new();

fn main() -> ExitCode {
	// The unwinder must know where the unwind tables are before anything can panic.
	#[cfg(target_os = "freebsd")]
	rac_ps4_shim::init();
	// Before any thread exists, so the profiler knows them all.
	#[cfg(target_os = "freebsd")]
	prof::hook();
	#[cfg(target_os = "freebsd")]
	ps4::start();
	#[cfg(target_os = "freebsd")]
	prof::start();
	#[cfg(windows)]
	prof::start_from_env();
	rac_game::set_register(ee_game::register);
	let code = rac_game::main();
	// Returning from main crashes on the PS4 (CE-34878-0): quit through the system service.
	#[cfg(target_os = "freebsd")]
	{
		eprintln!("ps4: the game returned {code:?}; {}", ps4::mem_line());
		ps4::sync_log();
		rac_ps4_shim::quit(0);
	}
	#[allow(unreachable_code)]
	code
}

#[cfg(target_os = "freebsd")]
mod ps4 {
	use std::io::Write;

	/// The console's writable folder for this app (reachable by FTP): the disc image, card.bin, the log, env.txt.
	pub const DIR: &str = "/data/rac-rs";

	/// The scripted pad of the PS4_GPU_DESIGN.md step 5 recipe (USA disc, cold boot): Start at the title, Down + Cross to Load Game,
	/// Cross loads the first save of card.bin; then walk forward (left stick up) for a while. There is no controller input yet.
	const PAD: &str = "220-226:800:80,80;280-286:4000:80,80;320-326:40:80,80;420-426:40:80,80;1300-2900:0:80,0";

	unsafe extern "C" {
		// From our patched std (ps4/std.patch, sys/stdio/unix.rs): stdout and stderr write to this descriptor.
		fn rac_std_set_log_fd(fd: i32);
		fn rac_std_sync_log() -> i32;
		// Also ours (sys/thread/unix.rs): the core count `available_parallelism` reports, when non-zero.
		fn rac_std_set_cpus(n: usize);
		fn sceKernelAvailableFlexibleMemorySize(size: *mut usize) -> i32;
		fn sceKernelGetDirectMemorySize() -> usize;
	}

	/// Forces the log to disk. The console caches file writes, and the first console run, which froze the console, came back
	/// with an empty log.txt: whatever matters must be written out before a possible freeze.
	pub fn sync_log() {
		// SAFETY: fsync of the log descriptor std holds (nothing before `start` hands it over).
		unsafe { rac_std_sync_log() };
	}

	pub fn mem_line() -> String {
		let mut flex = 0usize;
		// SAFETY: plain out-parameter calls into libkernel.
		let r = unsafe { sceKernelAvailableFlexibleMemorySize(&mut flex) };
		let direct = unsafe { sceKernelGetDirectMemorySize() };
		let err = super::heap::LAST_ERR.load(std::sync::atomic::Ordering::Relaxed);
		format!(
			"flexible memory free {} MB (r={r:#x}), heap {} MB of direct memory {} MB{}",
			flex >> 20,
			super::heap::MAPPED.load(std::sync::atomic::Ordering::Relaxed) >> 20,
			direct >> 20,
			if err != 0 { format!(", last heap error {err:#x}") } else { String::new() }
		)
	}

	/// The test series' run for this start: N from runs/next.txt (1 if missing) when runs/runN.txt exists; next.txt is moved on
	/// at once, so a run that hangs or crashes still lets the next start take the next run.
	fn next_run() -> Option<u32> {
		let dir = format!("{DIR}/runs");
		let n: u32 = std::fs::read_to_string(format!("{dir}/next.txt")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(1);
		if !std::path::Path::new(&format!("{dir}/run{n}.txt")).is_file() {
			return None;
		}
		let _ = std::fs::write(format!("{dir}/next.txt"), format!("{}
", n + 1));
		Some(n)
	}

	/// KEY=VALUE lines of `path` into the environment (`KEY=` removes one, `#` starts a comment).
	fn apply_env(path: &str, tag: &str) {
		let Ok(s) = std::fs::read_to_string(path) else { return };
		for line in s.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
			let Some((k, v)) = line.split_once('=') else { continue };
			let (k, v) = (k.trim(), v.trim());
			// SAFETY: single-threaded, before the game starts any thread.
			if v.is_empty() {
				unsafe { std::env::remove_var(k) };
			} else {
				unsafe { std::env::set_var(k, v) };
			}
			eprintln!("ps4: {tag} {k}={v}");
		}
	}

	/// `path` written and forced to disk (the console caches writes; the player may close the app with the PS button).
	fn write_synced(path: &str, text: &str) -> std::io::Result<()> {
		let mut f = std::fs::File::create(path)?;
		f.write_all(text.as_bytes())?;
		f.sync_all()
	}

	/// `RAC_PS4_PCOPT=shadows=off,ao=off` (a run file or env.txt): those graphics options for this start only. Before the first
	/// such test start the player's pc_options.txt is kept as pc_options.orig.txt, and test starts build on that copy. The first
	/// normal start puts it back and deletes it, so from then on every normal start keeps what the player chose in the game's
	/// Options (which write pc_options.txt); only choices made during a test start are dropped with the test's options.
	/// The game also applies RAC_PS4_PCOPT itself when it reads the file (rac-rs retail/pcopts.rs `PcOptions::load`): round 5's
	/// run 5 logged "graphics option shadows=off" here and still "dynamic shadows high" from the game, so the file is also read
	/// back and logged.
	fn pc_options_for_run() {
		let (cur, orig) = (format!("{DIR}/pc_options.txt"), format!("{DIR}/pc_options.orig.txt"));
		let set = std::env::var("RAC_PS4_PCOPT").unwrap_or_default();
		// Plain reads and writes (no fs::copy), each failure logged.
		// An empty pc_options.orig.txt counts as none: the stash is emptied before it is deleted, because a delete that silently
		// does nothing (shadPS4's unlink reports success and keeps the file) must not bring old options back at a later start.
		let stash = std::fs::read_to_string(&orig).ok().filter(|t| !t.trim().is_empty());
		if set.trim().is_empty() {
			if let Some(t) = stash {
				match write_synced(&cur, &t) {
					Ok(()) => {
						let _ = write_synced(&orig, "");
						let _ = std::fs::remove_file(&orig);
						eprintln!("ps4: a normal start: the test options are gone, {cur} is the player's own again");
					}
					Err(e) => eprintln!("ps4: could not write {cur}: {e}"),
				}
			}
			return;
		}
		let base = match stash {
			Some(t) => t,
			None => {
				let t = std::fs::read_to_string(&cur).unwrap_or_default();
				if let Err(e) = write_synced(&orig, &t) {
					eprintln!("ps4: could not write {orig}: {e}");
				}
				t
			}
		};
		let mut text = base;
		for kv in set.split(',').filter_map(|p| p.trim().split_once('=')) {
			let (k, v) = (kv.0.trim(), kv.1.trim());
			let mut lines: Vec<String> = text.lines().filter(|l| l.split_once('=').map(|x| x.0.trim()) != Some(k)).map(String::from).collect();
			lines.push(format!("{k}={v}"));
			text = lines.join("\n") + "\n";
			eprintln!("ps4: graphics option {k}={v} for this start");
		}
		if std::fs::read_to_string(&cur).ok().as_deref() != Some(text.as_str()) {
			if let Err(e) = write_synced(&cur, &text) {
				eprintln!("ps4: could not write {cur}: {e}");
			}
		}
		let back = std::fs::read_to_string(&cur).unwrap_or_else(|e| format!("(unreadable: {e})"));
		eprintln!("ps4: {cur} now: {}", back.lines().collect::<Vec<_>>().join(", "));
	}

	/// Sets everything up before the game's `main` (still single-threaded, so `set_var` is sound).
	pub fn start() {
		let _ = std::fs::create_dir_all(DIR);
		// The run log: stdout and stderr into log.txt (the previous run's is kept as log.prev.txt), as the PC's run log does.
		// A test series (/data/rac-rs/runs/run1.txt, run2.txt, ...): each start of the app takes the next run file and logs into
		// its own log_runN.txt, so the player copies one folder over once, starts the app N times and copies everything back.
		let run = next_run();
		let log = match run {
			Some(n) => format!("{DIR}/log_run{n}.txt"),
			None => format!("{DIR}/log.txt"),
		};
		let _ = std::fs::rename(&log, log.replace(".txt", ".prev.txt"));
		// Copying the file onto descriptors 1 and 2 (dup2) left the log empty on the console (runs 1 and 1b), so our std writes
		// stdout and stderr to the file's own descriptor instead, kept open for the whole run.
		let fd = std::fs::File::create(&log).map(|f| {
			use std::os::fd::IntoRawFd;
			f.into_raw_fd()
		});
		match fd {
			// SAFETY: a descriptor this process owns and never closes.
			Ok(fd) => unsafe { rac_std_set_log_fd(fd) },
			Err(e) => {
				let _ = std::fs::write(format!("{DIR}/log_error.txt"), format!("could not create {log}: {e}
"));
			}
		}
		eprintln!("ps4: rac-rs PS4 app starting; {}", mem_line());
		// First start: the graphics options and mods the shader cache was captured with (ps4/app/seed, in the PKG as /app0/seed_*,
		// flat because the PKG tools take files only in their own folders).
		// Other settings make other pipelines, which the cache may not have (they go to missing_pipelines.txt and are not drawn).
		for f in ["pc_options.txt", "mods/active.txt", "mods/settings.json"] {
			let to = format!("{DIR}/{f}");
			if !std::path::Path::new(&to).exists() {
				let _ = std::fs::create_dir_all(std::path::Path::new(&to).parent().unwrap_or(std::path::Path::new(DIR)));
				match std::fs::copy(format!("/app0/seed_{}", f.replace('/', "_")), &to) {
					Ok(_) => eprintln!("ps4: first start, {to} from the package"),
					Err(e) => eprintln!("ps4: could not write {to}: {e}"),
				}
			}
		}
		let skip = std::fs::read_to_string("/app0/hand_skip.txt").ok().and_then(|s| s.lines().next().map(|l| l.trim().to_string())).unwrap_or_default();
		let defaults: [(&str, String); 9] = [
			// The mods folder (on the PC it is next to the install; here the writable folder).
			("RAC_MODS_DIR", format!("{DIR}/mods")),
			("ORACLE_ISO", format!("{DIR}/game.iso")),
			("ORACLE_STUBS", "/app0/stubs.txt".into()),
			("ORACLE_HAND_SKIP", skip),
			("RAC_PLAYER_DIR", DIR.into()),
			// The game thread's stack: the PC's 256 MB does not fit the flexible memory (the probe runs with 32 MB).
			("ORACLE_EE_STACK_MB", "32".into()),
			// wgpu's noop slot is the GNM back end (its shader cache /app0/ps4shaders.bin, misses into missing_pipelines.txt).
			("WGPU_BACKEND", "noop".into()),
			("WGPU_NOOP_BACKEND", "1".into()),
			// The frame worker pool (rayon): the console has 7 cores for the app; the EE thread needs one to itself.
			("RAC_THREADS", "4".into()),
		];
		// SAFETY (all set_var / remove_var below): single-threaded, before the game starts any thread.
		for (k, v) in &defaults {
			unsafe { std::env::set_var(k, v) };
		}
		// /data/rac-rs/env.txt: KEY=VALUE lines that override the defaults or add switches (`KEY=` removes one, `#` starts a comment),
		// so a test can change the pad script, stop after N frames (RAC_PS4_FRAMES) etc. without a new PKG.
		apply_env(&format!("{DIR}/env.txt"), "env.txt");
		match run {
			Some(n) => {
				eprintln!("ps4: test series, run {n} (the next start takes run {})", n + 1);
				// The scripted pad (loads the first save, walks forward for fields 1300..2900) only in a test series, before the run
				// file (which can replace it or remove it with `RAC_RETAIL_PAD=`). It used to be on for every start, so a player's
				// normal start got the script's presses and its forward walk mixed into the controller (free play 2026-11-10:
				// "locked into the forward movement" right after spawning).
				if std::env::var_os("RAC_RETAIL_PAD").is_none() {
					// SAFETY: single-threaded, before the game starts any thread.
					unsafe { std::env::set_var("RAC_RETAIL_PAD", PAD) };
				}
				apply_env(&format!("{DIR}/runs/run{n}.txt"), &format!("run{n}.txt"));
				if std::env::var_os("RAC_PS4_PROF_OUT").is_none() {
					// SAFETY: single-threaded, before the game starts any thread.
					unsafe { std::env::set_var("RAC_PS4_PROF_OUT", format!("{DIR}/profile_run{n}.txt")) };
				}
				// Each run's pictures in a folder of their own, so the runs do not overwrite each other's.
				if std::env::var_os("RAC_PS4_SHOT_DIR").is_none() {
					let d = format!("{DIR}/shots_run{n}");
					let _ = std::fs::create_dir_all(&d);
					// SAFETY: single-threaded, before the game starts any thread.
					unsafe { std::env::set_var("RAC_PS4_SHOT_DIR", &d) };
				}
			}
			None if std::path::Path::new(&format!("{DIR}/runs")).is_dir() => eprintln!("ps4: test series finished (no runs/run file left); a normal start"),
			None => {}
		}
		pc_options_for_run();
		// The core count decides how many helper threads the game starts (GS capture, encoder finishes, picture tuner...); when
		// std cannot ask the system it says 1 and most of that work stays on one thread. RAC_PS4_CPUS=N forces N.
		let reported = std::thread::available_parallelism();
		let forced = std::env::var("RAC_PS4_CPUS").ok().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0);
		if let Some(n) = forced {
			// SAFETY: a plain store in our std.
			unsafe { rac_std_set_cpus(n) };
		}
		eprintln!("ps4: cores: std reports {reported:?}{}", forced.map(|n| format!(", forced to {n} (RAC_PS4_CPUS)")).unwrap_or_default());
		let iso = std::env::var("ORACLE_ISO").unwrap_or_default();
		if !std::path::Path::new(&iso).is_file() {
			eprintln!("ps4: no disc image at {iso} (copy your USA or PAL disc image there as game.iso)");
		}
		rac_game::set_args(["view", iso.as_str(), "0", "--retail-cold", "--modern-cam"].map(String::from).to_vec());
		// A panic anywhere lands in the log (stderr is the log), with the thread's name.
		let prev = std::panic::take_hook();
		std::panic::set_hook(Box::new(move |p| {
			eprintln!("ps4: PANIC in thread {}: {p}", std::thread::current().name().unwrap_or("?"));
			prev(p);
			sync_log();
		}));
		sync_log();
		// The log forced to disk every second, and memory every 10 s (the flexible-memory budget and the heap's growth).
		let _ = std::thread::Builder::new().name("ps4-mem".into()).stack_size(256 << 10).spawn(|| {
			let t0 = std::time::Instant::now();
			for n in 1u64.. {
				std::thread::sleep(std::time::Duration::from_secs(1));
				if n % 10 == 0 {
					eprintln!("ps4: {:.0}s {}", t0.elapsed().as_secs_f64(), mem_line());
				}
				let _ = std::io::stderr().flush();
				sync_log();
			}
		});
	}
}
