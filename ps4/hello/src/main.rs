//! PS4 std smoke test: writes /data/rac-rs/hello.log with one line per check.
use std::io::Write;

// The C functions the PS4 lacks (ps4/shim).
use rac_ps4_shim as _;

#[cfg(target_os = "windows")]
const DIR: &str = ".";
#[cfg(not(target_os = "windows"))]
const DIR: &str = "/data/rac-rs";

fn main() {
	// The unwinder must know where the unwind tables are before anything can panic (ps4/shim).
	#[cfg(target_os = "freebsd")]
	rac_ps4_shim::init();
	let _ = std::fs::create_dir_all(DIR);
	let mut f = std::fs::File::create(format!("{DIR}/hello.log")).expect("log file");
	let mut say = |s: &str| {
		let _ = writeln!(f, "{s}");
		let _ = f.flush();
	};
	say("hello: std is alive");
	let t = std::time::Instant::now();
	let m: std::collections::HashMap<u32, u32> = (0..1000).map(|i| (i, i * 2)).collect();
	say(&format!("hello: hash map {}", m[&500]));
	// The core's game thread asks for a 256 MB stack.
	let (tx, rx) = std::sync::mpsc::channel();
	let j = std::thread::Builder::new()
		.stack_size(256 << 20)
		.spawn(move || {
			let big = [7u8; 1 << 20];
			tx.send(big.iter().map(|&b| b as u64).sum::<u64>()).unwrap();
		})
		.map(|j| j.join());
	say(&format!("hello: 256 MB stack thread {:?}, sent {:?}", j.is_ok(), rx.recv().ok()));
	// The recompiled code leaves functions by panicking and catching (rec rebuilds): unwinding must work.
	let c = std::panic::catch_unwind(|| panic!("on purpose"));
	say(&format!("hello: caught panic {}", c.is_err()));
	let v = vec![0u8; 32 << 20];
	say(&format!("hello: 32 MB vec {}", v.len()));
	let mx = std::sync::Mutex::new(0);
	std::thread::scope(|s| {
		for _ in 0..4 {
			s.spawn(|| {
				for _ in 0..100_000 {
					*mx.lock().unwrap() += 1;
				}
			});
		}
	});
	say(&format!("hello: mutex count {} (400000), {} cpus", mx.lock().unwrap(), std::thread::available_parallelism().map_or(0, |n| n.get())));
	say(&format!("hello: iso readable {}", std::fs::metadata(format!("{DIR}/game.iso")).map(|m| m.len()).unwrap_or(0)));
	say(&format!("hello: done in {:?}", t.elapsed()));
	// Returning from main crashes on the console (CE-34878-0); quit the PS4 way (ps4/shim).
	#[cfg(target_os = "freebsd")]
	rac_ps4_shim::quit(0);
}
