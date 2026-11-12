//! The console profiler: `RAC_PS4_PROF=START,SECONDS` (env.txt or a run file) samples every thread of the app for SECONDS,
//! starting START seconds after launch, and writes `RAC_PS4_PROF_OUT` (default /data/rac-rs/profile.txt). Same text format as
//! the PC's (prof_win.rs), so `sh ps4/prof.sh profile.txt build-ps4/app/app.elf` makes the per-thread table.
//!
//! How: std's spawn hook (`add_spawn_hook`, set first thing in main, inherited by every thread spawned after it) notes each new
//! thread's pthread and name. A sampler thread sends SIGPROF to each noted thread in turn (`pthread_kill`) every 4 ms; the
//! handler, running on that thread, notes the interrupted instruction address, the word at the top of its stack (the caller,
//! when it stopped in a leaf such as a libkernel call), the core it ran on (`rdtscp`'s TSC_AUX, when the kernel sets it) and
//! its own CPU clock. A thread that waits shows libkernel addresses only ("outside the game"); its CPU clock tells it apart
//! from one that works. Off unless the variable is set: signals interrupt system calls, so a profiled run is a run of its own.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

const SIGPROF: i32 = 27;
const SA_SIGINFO: i32 = 0x40;
const SA_RESTART: i32 = 0x2;
/// FreeBSD's CLOCK_THREAD_CPUTIME_ID.
const CLOCK_THREAD_CPUTIME_ID: i32 = 14;
const MAX: usize = 600_000;
const THREADS: usize = 96;
const NAME: usize = 32;

/// FreeBSD's `struct sigaction` (amd64).
#[repr(C)]
struct SigAction {
	handler: usize,
	flags: i32,
	mask: [u32; 4],
}

#[repr(C)]
struct Timespec {
	sec: i64,
	nsec: i64,
}

unsafe extern "C" {
	fn sigaction(sig: i32, act: *const SigAction, old: *mut SigAction) -> i32;
	fn pthread_self() -> u64;
	fn pthread_kill(t: u64, sig: i32) -> i32;
	fn clock_gettime(clock: i32, ts: *mut Timespec) -> i32;
}

/// The PS4's signal context: the interrupted rip is word 28 and rsp word 31 (found on the console for the probe's profiler).
const RIP_WORD: usize = 28;
const RSP_WORD: usize = 31;

// Noted threads: pthread id (0 = free slot), name, CPU clock (ns) at the first and the latest sample.
static TID: [AtomicU64; THREADS] = [const { AtomicU64::new(0) }; THREADS];
static mut NAMES: [[u8; NAME]; THREADS] = [[0; NAME]; THREADS];
static CPU_FIRST: [AtomicU64; THREADS] = [const { AtomicU64::new(0) }; THREADS];
static CPU_LAST: [AtomicU64; THREADS] = [const { AtomicU64::new(0) }; THREADS];
static NTHREADS: AtomicUsize = AtomicUsize::new(0);

// Samples: instruction address, caller word, thread slot << 8 | core.
static mut RIP: [u64; MAX] = [0; MAX];
static mut RET: [u64; MAX] = [0; MAX];
static mut WHO: [u16; MAX] = [0; MAX];
static N: AtomicUsize = AtomicUsize::new(0);
static ON: AtomicBool = AtomicBool::new(false);

/// The address prof.sh subtracts to undo the console's load address (position-independent executable).
#[inline(never)]
pub fn anchor() -> usize {
	anchor as *const () as usize
}

fn cpu_ns() -> u64 {
	let mut ts = Timespec { sec: 0, nsec: 0 };
	// SAFETY: valid out pointer; async-signal-safe.
	if unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut ts) } != 0 {
		return 0;
	}
	ts.sec as u64 * 1_000_000_000 + ts.nsec as u64
}

/// The core this instruction runs on, as the kernel leaves it in TSC_AUX (0 when it does not set it).
fn core() -> u32 {
	let aux: u32;
	// SAFETY: rdtscp is allowed in user mode on Jaguar; it only reads the counter and TSC_AUX.
	unsafe { core::arch::asm!("rdtscp", out("eax") _, out("edx") _, out("ecx") aux, options(nomem, nostack)) };
	aux
}

/// Notes the calling thread under `name`.
fn note(name: &str) {
	let me = unsafe { pthread_self() };
	let i = NTHREADS.fetch_add(1, Ordering::Relaxed);
	if i >= THREADS {
		return;
	}
	let b = name.as_bytes();
	let n = b.len().min(NAME - 1);
	// SAFETY: slot `i` belongs to this thread alone until TID[i] is published below.
	unsafe {
		let dst = &mut (*core::ptr::addr_of_mut!(NAMES))[i];
		dst[..n].copy_from_slice(&b[..n]);
		for c in &mut dst[..n] {
			if *c == b' ' {
				*c = b'_';
			}
		}
	}
	TID[i].store(me, Ordering::Release);
	// The slot is cleared when the thread ends (its thread-locals are dropped before it is gone), so the sampler never signals a
	// thread that no longer exists (shadPS4 asserts on that; libkernel would hand back an error at best).
	struct Leave(usize);
	impl Drop for Leave {
		fn drop(&mut self) {
			TID[self.0].store(0, Ordering::Release);
		}
	}
	thread_local! {
		static LEAVE: std::cell::Cell<Option<Leave>> = const { std::cell::Cell::new(None) };
	}
	if name != "main" {
		LEAVE.with(|l| l.set(Some(Leave(i))));
	}
}

fn slot_of(me: u64) -> Option<usize> {
	let n = NTHREADS.load(Ordering::Relaxed).min(THREADS);
	(0..n).find(|&i| TID[i].load(Ordering::Relaxed) == me)
}

extern "C" fn on_prof(_sig: i32, _info: *mut u8, uc: *mut u8) {
	if !ON.load(Ordering::Relaxed) {
		return;
	}
	let Some(slot) = slot_of(unsafe { pthread_self() }) else { return };
	let cpu = cpu_ns();
	let _ = CPU_FIRST[slot].compare_exchange(0, cpu, Ordering::Relaxed, Ordering::Relaxed);
	CPU_LAST[slot].store(cpu, Ordering::Relaxed);
	let i = N.fetch_add(1, Ordering::Relaxed);
	if i >= MAX {
		return;
	}
	let here = &i as *const usize as u64;
	// SAFETY: the kernel hands a valid context; the stack word is read only inside this thread's live stack (just above this
	// handler's own frame, which runs on the interrupted stack).
	unsafe {
		let w = uc as *const u64;
		let rip = *w.add(RIP_WORD);
		let sp = *w.add(RSP_WORD);
		RIP[i] = rip;
		RET[i] = if sp > here && sp - here < 1 << 20 && sp % 8 == 0 { *(sp as *const u64) } else { 0 };
		WHO[i] = ((slot as u16) << 8) | (core() as u16 & 0xff);
	}
}

/// First thing in main: the spawn hook (every later thread is noted) and the main thread itself. Does nothing unless
/// RAC_PS4_PROF is set (read after env.txt / the run file are applied, so `start` decides; the hook itself is cheap).
pub fn hook() {
	note("main");
	std::thread::add_spawn_hook(|t| {
		let name = t.name().unwrap_or("unnamed").to_string();
		move || note(&name)
	});
}

/// After env.txt and the run file: starts the sampler when RAC_PS4_PROF asks for it.
pub fn start() {
	let Ok(v) = std::env::var("RAC_PS4_PROF") else { return };
	let mut it = v.split(',').map(|s| s.trim().parse::<f64>().unwrap_or(0.0));
	let (start, len) = (it.next().unwrap_or(60.0), it.next().unwrap_or(30.0).max(1.0));
	let out = std::env::var("RAC_PS4_PROF_OUT").unwrap_or_else(|_| "/data/rac-rs/profile.txt".into());
	let act = SigAction { handler: on_prof as *const () as usize, flags: SA_SIGINFO | SA_RESTART, mask: [0; 4] };
	// SAFETY: plain libkernel call with valid pointers.
	let r = unsafe { sigaction(SIGPROF, &act, std::ptr::null_mut()) };
	eprintln!("prof: sampling every thread from {start} s for {len} s into {out} (sigaction {r}, core now {})", core());
	let _ = std::thread::Builder::new().name("prof".into()).stack_size(256 << 10).spawn(move || {
		std::thread::sleep(std::time::Duration::from_secs_f64(start));
		let me = unsafe { pthread_self() };
		ON.store(true, Ordering::Relaxed);
		let t0 = std::time::Instant::now();
		let mut kills = 0u64;
		let mut fails = 0u64;
		while t0.elapsed().as_secs_f64() < len && N.load(Ordering::Relaxed) < MAX {
			std::thread::sleep(std::time::Duration::from_micros(4000));
			let n = NTHREADS.load(Ordering::Relaxed).min(THREADS);
			for i in 0..n {
				let t = TID[i].load(Ordering::Acquire);
				if t != 0 && t != me {
					kills += 1;
					// A thread that ended makes pthread_kill fail (ESRCH); its slot is dropped.
					if unsafe { pthread_kill(t, SIGPROF) } != 0 {
						fails += 1;
						TID[i].store(0, Ordering::Relaxed);
					}
				}
			}
		}
		ON.store(false, Ordering::Relaxed);
		let secs = t0.elapsed().as_secs_f64();
		write(&out, secs, kills, fails);
	});
}

fn write(path: &str, secs: f64, kills: u64, fails: u64) {
	let n = N.load(Ordering::Relaxed).min(MAX);
	let nt = NTHREADS.load(Ordering::Relaxed).min(THREADS);
	// SAFETY: sampling is off; nothing writes the arrays any more (a late handler returns at the ON check).
	let (rip, ret, who, names) = unsafe {
		(
			&(&*core::ptr::addr_of!(RIP))[..n],
			&(&*core::ptr::addr_of!(RET))[..n],
			&(&*core::ptr::addr_of!(WHO))[..n],
			&*core::ptr::addr_of!(NAMES),
		)
	};
	let name = |i: usize| {
		let b = &names[i];
		let e = b.iter().position(|&c| c == 0).unwrap_or(NAME);
		String::from_utf8_lossy(&b[..e]).into_owned()
	};
	let mut s = String::with_capacity(n * 40 + 4096);
	s.push_str(&format!("anchor {:x}\n", anchor()));
	s.push_str(&format!("# {n} samples over {secs:.1} s, {kills} signals sent, {fails} failed; available_parallelism {:?}\n", std::thread::available_parallelism()));
	for i in 0..nt {
		let mut cores = [0u32; 16];
		let mut cnt = 0;
		for &w in who {
			if (w >> 8) as usize == i {
				cores[(w & 15) as usize] += 1;
				cnt += 1;
			}
		}
		let busy = CPU_LAST[i].load(Ordering::Relaxed).saturating_sub(CPU_FIRST[i].load(Ordering::Relaxed)) as f64 / 1e9;
		let spread: Vec<String> = cores.iter().enumerate().filter(|&(_, &c)| c > 0).map(|(k, c)| format!("{k}:{c}")).collect();
		s.push_str(&format!("# thread {} cpu {:.1}% samples {cnt} cores {}\n", name(i), 100.0 * busy / secs, spread.join(",")));
	}
	for k in 0..n {
		let slot = (who[k] >> 8) as usize;
		s.push_str(&format!("{:x} {:x} {} {}\n", rip[k], ret[k], name(slot), who[k] & 0xff));
	}
	let r = std::fs::write(path, s);
	eprintln!("prof: {n} samples of {nt} threads over {secs:.1} s written to {path} ({r:?})");
}
