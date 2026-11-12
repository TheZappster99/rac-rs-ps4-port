//! PC twin of the console profiler (prof.rs): `RAC_PS4_PROF=START,SECONDS` samples every named thread of the process about a
//! thousand times a second for SECONDS, starting START seconds after launch. Each sample suspends one thread, reads its
//! instruction pointer and the word at the top of its stack (the caller, when it stopped in a leaf), and resumes it. Each
//! thread's CPU time over the window is noted too, so a thread that sleeps (and only shows wait functions) is told apart from
//! one that works. Written to `$RAC_PLAYER_DIR/profile.txt`; `sh ps4/prof.sh profile.txt build-ps4/target/release/rac-ps4-app.exe`
//! turns it into a table per thread.

use std::sync::atomic::{AtomicBool, Ordering};

#[repr(C)]
struct ThreadEntry32 {
	size: u32,
	usage: u32,
	tid: u32,
	pid: u32,
	base_pri: i32,
	delta_pri: i32,
	flags: u32,
}

/// x64 `CONTEXT` (1232 bytes, 16-byte aligned): ContextFlags at 0x30, Rsp at 0x98, Rip at 0xF8.
#[repr(C, align(16))]
struct Context([u8; 1232]);

unsafe extern "system" {
	fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
	fn Thread32First(snap: isize, e: *mut ThreadEntry32) -> i32;
	fn Thread32Next(snap: isize, e: *mut ThreadEntry32) -> i32;
	fn CloseHandle(h: isize) -> i32;
	fn OpenThread(access: u32, inherit: i32, tid: u32) -> isize;
	fn GetThreadDescription(h: isize, desc: *mut *mut u16) -> i32;
	fn LocalFree(p: *mut u16) -> *mut u16;
	fn GetCurrentProcessId() -> u32;
	fn GetCurrentThreadId() -> u32;
	fn SuspendThread(h: isize) -> u32;
	fn ResumeThread(h: isize) -> u32;
	fn GetThreadContext(h: isize, c: *mut Context) -> i32;
	fn GetThreadTimes(h: isize, c: *mut u64, e: *mut u64, k: *mut u64, u: *mut u64) -> i32;
	fn ReadProcessMemory(p: isize, addr: usize, buf: *mut u64, n: usize, read: *mut usize) -> i32;
	fn GetCurrentProcess() -> isize;
}

static RUN: AtomicBool = AtomicBool::new(true);

/// The address prof.sh subtracts (the exe may be loaded elsewhere than its preferred base).
#[inline(never)]
pub fn anchor() -> usize {
	anchor as *const () as usize
}

struct Thread {
	tid: u32,
	h: isize,
	name: String,
	/// Kernel + user time (100 ns units) when the window started.
	cpu0: u64,
}

fn cpu_time(h: isize) -> u64 {
	let (mut c, mut e, mut k, mut u) = (0, 0, 0, 0);
	// SAFETY: valid handle and out pointers.
	unsafe { GetThreadTimes(h, &mut c, &mut e, &mut k, &mut u) };
	k + u
}

/// Adds the process's threads not yet in `list` (every thread; unnamed ones are called `tid<N>`).
fn refresh(list: &mut Vec<Thread>) {
	// SAFETY: plain Win32 calls with valid, sized structs.
	unsafe {
		let snap = CreateToolhelp32Snapshot(4, 0);
		let me = GetCurrentProcessId();
		let self_tid = GetCurrentThreadId();
		let mut e = ThreadEntry32 { size: std::mem::size_of::<ThreadEntry32>() as u32, usage: 0, tid: 0, pid: 0, base_pri: 0, delta_pri: 0, flags: 0 };
		let mut ok = Thread32First(snap, &mut e);
		while ok != 0 {
			if e.pid == me && e.tid != self_tid && !list.iter().any(|t| t.tid == e.tid) {
				let h = OpenThread(0x1F_FFFF, 0, e.tid);
				if h != 0 {
					let mut name = format!("tid{}", e.tid);
					let mut d: *mut u16 = std::ptr::null_mut();
					if GetThreadDescription(h, &mut d) >= 0 && !d.is_null() {
						let mut k = 0;
						while *d.add(k) != 0 {
							k += 1;
						}
						if k > 0 {
							name = String::from_utf16_lossy(std::slice::from_raw_parts(d, k));
						}
						LocalFree(d);
					}
					// Names with spaces would break the profile's columns.
					let name = name.replace(' ', "_");
					list.push(Thread { tid: e.tid, h, name, cpu0: cpu_time(h) });
				}
			}
			ok = Thread32Next(snap, &mut e);
		}
		CloseHandle(snap);
	}
}

pub fn start_from_env() {
	let Ok(v) = std::env::var("RAC_PS4_PROF") else { return };
	let mut it = v.split(',').map(|s| s.trim().parse::<f64>().unwrap_or(0.0));
	let (start, len) = (it.next().unwrap_or(30.0), it.next().unwrap_or(30.0).max(1.0));
	let dir = std::env::var("RAC_PLAYER_DIR").unwrap_or_else(|_| ".".into());
	let path = format!("{dir}/profile.txt");
	eprintln!("prof: sampling every thread from {start} s for {len} s into {path}");
	let _ = std::thread::Builder::new().name("prof".into()).spawn(move || {
		std::thread::sleep(std::time::Duration::from_secs_f64(start));
		let mut threads = Vec::new();
		refresh(&mut threads);
		let mut samples: Vec<(u16, u64, u64)> = Vec::with_capacity(4_000_000);
		let mut c = Context([0; 1232]);
		let proc_ = unsafe { GetCurrentProcess() };
		let t0 = std::time::Instant::now();
		let mut ticks = 0u64;
		while RUN.load(Ordering::Relaxed) && t0.elapsed().as_secs_f64() < len {
			std::thread::sleep(std::time::Duration::from_micros(1000));
			ticks += 1;
			if ticks % 500 == 0 {
				refresh(&mut threads);
			}
			for (i, t) in threads.iter().enumerate() {
				// SAFETY: the thread is suspended while its context is read; its stack top is read with ReadProcessMemory,
				// which fails cleanly on a bad address.
				unsafe {
					if SuspendThread(t.h) == u32::MAX {
						continue;
					}
					c.0[0x30..0x34].copy_from_slice(&0x0010_0001u32.to_le_bytes());
					let ok = GetThreadContext(t.h, &mut c);
					ResumeThread(t.h);
					if ok == 0 {
						continue;
					}
					let rip = u64::from_le_bytes(c.0[0xF8..0x100].try_into().unwrap());
					let rsp = u64::from_le_bytes(c.0[0x98..0xA0].try_into().unwrap());
					let mut ret = 0u64;
					let mut got = 0usize;
					ReadProcessMemory(proc_, rsp as usize, &mut ret, 8, &mut got);
					samples.push((i as u16, rip, ret));
				}
			}
		}
		let secs = t0.elapsed().as_secs_f64();
		let mut s = format!("anchor {:x}\n", anchor());
		for t in &threads {
			let busy = (cpu_time(t.h).saturating_sub(t.cpu0)) as f64 / 1e7;
			s.push_str(&format!("# thread {} cpu {:.1}%\n", t.name, 100.0 * busy / secs));
		}
		for (i, a, b) in &samples {
			s.push_str(&format!("{a:x} {b:x} {}\n", threads[*i as usize].name));
		}
		let r = std::fs::write(&path, s);
		eprintln!("prof: {} samples of {} threads over {secs:.1} s written to {path} ({r:?})", samples.len(), threads.len());
		for t in &threads {
			unsafe { CloseHandle(t.h) };
		}
	});
}
