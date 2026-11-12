//! The PC twin of prof.rs, for comparing the console's profile with the same run on Windows: a sampler thread suspends the
//! game thread (found by its name, "ee") about a thousand times a second, reads its instruction and stack pointers and
//! resumes it. Same output format as prof.rs (ps4/prof.sh reads both; pass the PC exe as the ELF argument).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const MAX: usize = 400_000;

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
	fn SuspendThread(h: isize) -> u32;
	fn ResumeThread(h: isize) -> u32;
	fn GetThreadContext(h: isize, c: *mut Context) -> i32;
	fn ReadProcessMemory(p: isize, addr: usize, buf: *mut u64, n: usize, read: *mut usize) -> i32;
	fn GetCurrentProcess() -> isize;
}

static SAMPLES: std::sync::Mutex<Vec<(u64, u64)>> = std::sync::Mutex::new(Vec::new());
static N: AtomicUsize = AtomicUsize::new(0);
static ON: AtomicBool = AtomicBool::new(false);
static RUN: AtomicBool = AtomicBool::new(true);

#[inline(never)]
pub fn anchor() -> usize {
	anchor as *const () as usize
}

/// The handle of this process's thread named "ee" (the oracle's game thread).
fn find_ee() -> Option<isize> {
	// SAFETY: plain Win32 calls with valid, sized structs.
	unsafe {
		let snap = CreateToolhelp32Snapshot(4, 0);
		let me = GetCurrentProcessId();
		let mut e = ThreadEntry32 { size: std::mem::size_of::<ThreadEntry32>() as u32, usage: 0, tid: 0, pid: 0, base_pri: 0, delta_pri: 0, flags: 0 };
		let mut ok = Thread32First(snap, &mut e);
		let mut found = None;
		while ok != 0 {
			if e.pid == me {
				let h = OpenThread(0x1F_FFFF, 0, e.tid);
				if h != 0 {
					let mut d: *mut u16 = std::ptr::null_mut();
					if GetThreadDescription(h, &mut d) >= 0 && !d.is_null() {
						let mut k = 0;
						while *d.add(k) != 0 {
							k += 1;
						}
						let name = String::from_utf16_lossy(std::slice::from_raw_parts(d, k));
						LocalFree(d);
						if name == "ee" {
							found = Some(h);
							break;
						}
					}
					CloseHandle(h);
				}
			}
			ok = Thread32Next(snap, &mut e);
		}
		CloseHandle(snap);
		found
	}
}

pub fn start(say: &mut impl FnMut(String)) {
	let Some(h) = find_ee() else {
		say("probe: profiler: no thread named ee".into());
		return;
	};
	say("probe: profiler: sampling the ee thread".into());
	std::thread::Builder::new()
		.name("prof".into())
		.spawn(move || {
			let mut c = Context([0; 1232]);
			let proc_ = unsafe { GetCurrentProcess() };
			while RUN.load(Ordering::Relaxed) {
				std::thread::sleep(std::time::Duration::from_micros(1000));
				if !ON.load(Ordering::Relaxed) {
					continue;
				}
				// SAFETY: the thread is suspended while its context is read; its stack top is read with ReadProcessMemory,
				// which fails cleanly on a bad address.
				unsafe {
					if SuspendThread(h) == u32::MAX {
						continue;
					}
					c.0[0x30..0x34].copy_from_slice(&0x0010_0001u32.to_le_bytes());
					let ok = GetThreadContext(h, &mut c);
					ResumeThread(h);
					if ok == 0 {
						continue;
					}
					let rip = u64::from_le_bytes(c.0[0xF8..0x100].try_into().unwrap());
					let rsp = u64::from_le_bytes(c.0[0x98..0xA0].try_into().unwrap());
					let mut ret = 0u64;
					let mut got = 0usize;
					ReadProcessMemory(proc_, rsp as usize, &mut ret, 8, &mut got);
					let mut v = SAMPLES.lock().unwrap();
					if v.len() < MAX {
						v.push((rip, ret));
						N.fetch_add(1, Ordering::Relaxed);
					}
				}
			}
		})
		.expect("profiler thread");
}

pub fn set(on: bool) {
	ON.store(on, Ordering::Relaxed);
}

pub fn write(path: &str, say: &mut impl FnMut(String)) {
	ON.store(false, Ordering::Relaxed);
	RUN.store(false, Ordering::Relaxed);
	let v = SAMPLES.lock().unwrap();
	let mut s = format!("anchor {:x}\n", anchor());
	for (a, b) in v.iter() {
		s.push_str(&format!("{a:x} {b:x}\n"));
	}
	let r = std::fs::write(path, s);
	say(format!("probe: profiler: {} samples written to {path} ({r:?})", v.len()));
}
