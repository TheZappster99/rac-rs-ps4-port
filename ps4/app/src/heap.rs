//! The PS4 program heap: dlmalloc on direct memory. malloc (OpenOrbis musl) draws from the app's flexible memory, about 280 MB
//! for everything including thread stacks, and the game's CPU-side data (decoded levels, mirrors, caches) needs far more. Direct
//! memory is the big pool games map themselves (several GB; the GNM back end takes its 2 GB GPU heap from it too). The heap grows
//! in segments of at least `STEP`, mapped CPU read-write and write-back cached (type WB_ONION), and never gives memory back
//! (dlmalloc reuses what is freed inside its segments).
//!
//! One lock around one dlmalloc: a spin lock, because std's Mutex could itself allocate and a heap must not re-enter itself.

use std::alloc::{GlobalAlloc, Layout};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

unsafe extern "C" {
	fn sceKernelGetDirectMemorySize() -> usize;
	fn sceKernelAllocateDirectMemory(start: i64, end: i64, len: usize, align: usize, mem_type: i32, phys: *mut i64) -> i32;
	fn sceKernelMapDirectMemory(addr: *mut *mut c_void, len: usize, prot: i32, flags: i32, phys: i64, align: usize) -> i32;
	fn sceKernelReleaseDirectMemory(phys: i64, len: usize) -> i32;
}

/// Write-back, CPU-coherent memory (the kind the CPU reads fast; GARLIC write-combined memory is for the GPU).
const MEM_WB_ONION: i32 = 0;
const PROT_CPU_RW: i32 = 0x03;
/// Smallest segment mapped at a time, and its alignment.
const STEP: usize = 64 << 20;
const ALIGN: usize = 2 << 20;

/// Bytes of direct memory the heap has mapped so far (for the app's memory log line).
pub static MAPPED: AtomicUsize = AtomicUsize::new(0);
/// The last direct-memory error (0 = none), for the log.
pub static LAST_ERR: AtomicUsize = AtomicUsize::new(0);

struct Direct;

// SAFETY: hands out fresh, exclusively owned mappings; never frees them.
unsafe impl dlmalloc::Allocator for Direct {
	fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
		let len = size.max(STEP).next_multiple_of(ALIGN);
		let mut phys = 0i64;
		// SAFETY: plain libkernel calls with out-parameters we own.
		unsafe {
			let r = sceKernelAllocateDirectMemory(0, sceKernelGetDirectMemorySize() as i64, len, ALIGN, MEM_WB_ONION, &mut phys);
			if r != 0 {
				LAST_ERR.store(r as u32 as usize, Ordering::Relaxed);
				return (std::ptr::null_mut(), 0, 0);
			}
			let mut addr: *mut c_void = std::ptr::null_mut();
			let r = sceKernelMapDirectMemory(&mut addr, len, PROT_CPU_RW, 0, phys, ALIGN);
			if r != 0 || addr.is_null() {
				sceKernelReleaseDirectMemory(phys, len);
				LAST_ERR.store(r as u32 as usize, Ordering::Relaxed);
				return (std::ptr::null_mut(), 0, 0);
			}
			MAPPED.fetch_add(len, Ordering::Relaxed);
			(addr.cast(), len, 0)
		}
	}

	fn remap(&self, _ptr: *mut u8, _oldsize: usize, _newsize: usize, _can_move: bool) -> *mut u8 {
		std::ptr::null_mut()
	}

	fn free_part(&self, _ptr: *mut u8, _oldsize: usize, _newsize: usize) -> bool {
		false
	}

	fn free(&self, _ptr: *mut u8, _size: usize) -> bool {
		false
	}

	fn can_release_part(&self, _flags: u32) -> bool {
		false
	}

	fn allocates_zeros(&self) -> bool {
		false
	}

	fn page_size(&self) -> usize {
		16 << 10
	}
}

pub struct Heap {
	lock: AtomicBool,
	dl: std::cell::UnsafeCell<dlmalloc::Dlmalloc<Direct>>,
}

// SAFETY: the dlmalloc state is only touched with `lock` held.
unsafe impl Sync for Heap {}

impl Heap {
	pub const fn new() -> Heap {
		Heap { lock: AtomicBool::new(false), dl: std::cell::UnsafeCell::new(dlmalloc::Dlmalloc::new_with_allocator(Direct)) }
	}

	fn with<R>(&self, f: impl FnOnce(&mut dlmalloc::Dlmalloc<Direct>) -> R) -> R {
		let mut spins = 0u32;
		while self.lock.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
			spins += 1;
			if spins < 64 {
				std::hint::spin_loop();
			} else {
				// The holder may have been preempted on this core: let it run.
				std::thread::yield_now();
			}
		}
		// SAFETY: the lock is held.
		let r = f(unsafe { &mut *self.dl.get() });
		self.lock.store(false, Ordering::Release);
		r
	}
}

unsafe impl GlobalAlloc for Heap {
	unsafe fn alloc(&self, l: Layout) -> *mut u8 {
		self.with(|d| unsafe { d.malloc(l.size(), l.align()) })
	}

	unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
		self.with(|d| unsafe { d.calloc(l.size(), l.align()) })
	}

	unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
		self.with(|d| unsafe { d.free(p, l.size(), l.align()) })
	}

	unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
		self.with(|d| unsafe { d.realloc(p, l.size(), l.align(), new) })
	}
}
