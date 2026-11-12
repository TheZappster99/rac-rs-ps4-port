//! A small sampling profiler for the console (FreeBSD signals through libkernel). A sampler thread sends SIGPROF to the
//! process about a thousand times a second; only the game thread leaves it unblocked, so the handler runs there and notes
//! the interrupted instruction address plus the word at the top of its stack (the caller, when it stopped in a leaf such as
//! a libkernel call). The samples are written out as text with the run-time address of `anchor`; the PC turns them into
//! function names with the ELF's symbols (ps4/prof.sh).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const SIGPROF: i32 = 27;
const SIG_BLOCK: i32 = 1;
const SA_SIGINFO: i32 = 0x40;
const SA_RESTART: i32 = 0x2;
const MAX: usize = 400_000;

/// FreeBSD's `struct sigaction` (amd64).
#[repr(C)]
struct SigAction {
	handler: usize,
	flags: i32,
	mask: [u32; 4],
}

unsafe extern "C" {
	fn sigaction(sig: i32, act: *const SigAction, old: *mut SigAction) -> i32;
	fn pthread_sigmask(how: i32, set: *const [u32; 4], old: *mut [u32; 4]) -> i32;
	fn kill(pid: i32, sig: i32) -> i32;
	fn getpid() -> i32;
}

static mut RIP: [u64; MAX] = [0; MAX];
static mut RET: [u64; MAX] = [0; MAX];
static N: AtomicUsize = AtomicUsize::new(0);
static ON: AtomicBool = AtomicBool::new(false);
static RUN: AtomicBool = AtomicBool::new(true);

/// The PS4's signal context: the interrupted rip is word 28 and rsp word 31 (read from full context dumps on the console:
/// words 28..32 are rip, cs 0x43, rflags 0x2xx, rsp, ss 0x3b, the FreeBSD order six words later than FreeBSD's own
/// `ucontext_t`).
const RIP_WORD: usize = 28;
const RSP_WORD: usize = 31;

/// The handler: notes rip and the word at the top of the interrupted stack (the caller, when it stopped in a leaf). That
/// word is read only when rsp lies just above this handler's own frame (it runs on the interrupted thread's stack).
extern "C" fn on_prof(_sig: i32, _info: *mut u8, uc: *mut u8) {
	if !ON.load(Ordering::Relaxed) {
		return;
	}
	let i = N.fetch_add(1, Ordering::Relaxed);
	if i >= MAX {
		return;
	}
	let here = &i as *const usize as u64;
	// SAFETY: the kernel hands a valid context; the stack word is read only inside this thread's live stack.
	unsafe {
		let w = uc as *const u64;
		let rip = *w.add(RIP_WORD);
		let sp = *w.add(RSP_WORD);
		RIP[i] = rip;
		RET[i] = if sp > here && sp - here < 1 << 20 && sp % 8 == 0 { *(sp as *const u64) } else { 0 };
	}
}

/// The address the PC subtracts to undo the console's load address (position-independent executable).
#[inline(never)]
pub fn anchor() -> usize {
	anchor as *const () as usize
}

/// Installs the handler, blocks SIGPROF on this (the main) thread and starts the sampler thread. Call after the game thread
/// started: it was created with SIGPROF open, so it is the one thread that receives the signals.
pub fn start(say: &mut impl FnMut(String)) {
	let act = SigAction { handler: on_prof as *const () as usize, flags: SA_SIGINFO | SA_RESTART, mask: [0; 4] };
	let mut set = [0u32; 4];
	set[0] = 1 << (SIGPROF - 1);
	// SAFETY: plain libkernel calls with valid pointers.
	let (a, b) = unsafe { (sigaction(SIGPROF, &act, std::ptr::null_mut()), pthread_sigmask(SIG_BLOCK, &set, std::ptr::null_mut())) };
	say(format!("probe: profiler: sigaction {a}, sigmask {b}"));
	std::thread::Builder::new()
		.name("prof".into())
		.spawn(|| {
			let pid = unsafe { getpid() };
			while RUN.load(Ordering::Relaxed) {
				std::thread::sleep(std::time::Duration::from_micros(1000));
				if ON.load(Ordering::Relaxed) {
					unsafe { kill(pid, SIGPROF) };
				}
			}
		})
		.expect("profiler thread");
}

pub fn set(on: bool) {
	ON.store(on, Ordering::Relaxed);
}

/// Writes `anchor RIP` then one `rip ret` line per sample.
pub fn write(path: &str, say: &mut impl FnMut(String)) {
	ON.store(false, Ordering::Relaxed);
	RUN.store(false, Ordering::Relaxed);
	let n = N.load(Ordering::Relaxed).min(MAX);
	let mut s = String::with_capacity(n * 34 + 64);
	s.push_str(&format!("anchor {:x}\n", anchor()));
	for i in 0..n {
		// SAFETY: sampling is off; nothing writes the arrays any more.
		let (a, b) = unsafe { (RIP[i], RET[i]) };
		s.push_str(&format!("{a:x} {b:x}\n"));
	}
	let r = std::fs::write(path, s);
	say(format!("probe: profiler: {n} samples written to {path} ({r:?})"));
}
