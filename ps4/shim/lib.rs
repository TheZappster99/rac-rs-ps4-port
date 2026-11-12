//! The few C functions Rust's FreeBSD std calls that neither the PS4's libkernel nor the OpenOrbis libc has. A program links them
//! by naming the crate once (`use rac_ps4_shim as _;`); on other systems the crate is empty.
#![cfg(target_os = "freebsd")]

use core::ffi::{c_char, c_int, c_void};

/// std seeds its hash maps from this. The seed only needs to differ between runs, not be secret: the time-stamp counter run
/// through splitmix64.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arc4random_buf(buf: *mut u8, n: usize) {
	let mut s = unsafe { core::arch::x86_64::_rdtsc() } ^ (buf as u64);
	for i in 0..n {
		s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
		let mut z = s;
		z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
		z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
		unsafe { *buf.add(i) = (z ^ (z >> 31)) as u8 };
	}
}

unsafe extern "C" {
	fn pthread_set_name_np(thread: *mut c_void, name: *const c_char);
}

/// Newer FreeBSD's name for thread names; the PS4 has only the older `pthread_set_name_np`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_setname_np(thread: *mut c_void, name: *const c_char) -> c_int {
	unsafe { pthread_set_name_np(thread, name) };
	0
}

/// std's `Command` with a working directory (FreeBSD 13.1+). The PS4 cannot start processes anyway; failing with ENOSYS makes
/// std report an error instead of the PKG loader refusing the eboot for a missing symbol (the game links `Command` code).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn posix_spawn_file_actions_addchdir_np(_actions: *mut c_void, _path: *const c_char) -> c_int {
	78
}

/// The POSIX name of the same function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn posix_spawn_file_actions_addchdir(_actions: *mut c_void, _path: *const c_char) -> c_int {
	78
}

// The unwinder: `unwinding` exports the `_Unwind_*` functions std's panic machinery calls (build.sh no longer links
// libunwind.a). It is told where the unwind tables are: OpenOrbis's link.x marks the start of the code and of .eh_frame_hdr.
use unwinding::custom_eh_frame_finder::{EhFrameFinder, FrameInfo, FrameInfoKind, set_custom_eh_frame_finder};

unsafe extern "C" {
	static __text_start: u8;
	static __eh_frame_start: u8;
	static __eh_frame_hdr_start: u8;
}

struct Tables;

unsafe impl EhFrameFinder for Tables {
	fn find(&self, pc: usize) -> Option<FrameInfo> {
		// SAFETY: linker-defined symbols; only their addresses are taken.
		let (text, end, hdr) = unsafe {
			(&__text_start as *const u8 as usize, &__eh_frame_start as *const u8 as usize, &__eh_frame_hdr_start as *const u8 as usize)
		};
		// The code (and the read-only data after it) ends where .eh_frame begins.
		(text..end).contains(&pc).then_some(FrameInfo { text_base: Some(text), kind: FrameInfoKind::EhFrameHdr(hdr) })
	}
}

static TABLES: Tables = Tables;

/// Call first thing in `main`: before it, a panic finds no unwind tables and aborts.
pub fn init() {
	let _ = set_custom_eh_frame_finder(&TABLES);
}

unsafe extern "C" {
	fn sceSystemServiceLoadExec(path: *const c_char, args: *const *const c_char) -> c_int;
	fn _exit(code: c_int) -> !;
}

/// Ends the program the console's way. Returning from `main` (libc's `exit`, at-exit handlers, thread-key destructors) ended
/// every run so far with error CE-34878-0 after its work was done; `sceSystemServiceLoadExec("exit")` hands back to the home
/// screen, and the kernel's `_exit` is the fallback if that returns.
pub fn quit(code: i32) -> ! {
	// SAFETY: a NUL-terminated path and a null argument list.
	unsafe {
		sceSystemServiceLoadExec(c"exit".as_ptr(), core::ptr::null());
		_exit(code)
	}
}
