//! GPU memory for the GNM back end: two heaps carved out of direct memory with a first-fit free list.
//!   - main: buffers, textures, shader code, command buffers;
//!   - low (below 4 GB): bind groups (descriptor sets), vertex-buffer tables, push-constant copies and dynamic descriptors,
//!     which radv's GFX7 shaders read through 32-bit pointers.

use alloc::{collections::BTreeMap, format, string::String};

use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use parking_lot::Mutex;

use super::plat;

/// A heap: one mapped block and its free ranges (offset -> length), never touching each other. A map rather than a sorted
/// vector: freeing into a fragmented heap shifted the whole vector on every free (a hot spot on the render thread); the map
/// walks in the same offset order, so first fit hands out exactly the same ranges.
pub struct Heap {
	pub name: &'static str,
	block: plat::Block,
	free: Mutex<BTreeMap<usize, usize>>,
	/// Bytes in use now and the most ever (the high-water mark tells how close a game comes to the console's heap size).
	used: AtomicUsize,
	peak: AtomicUsize,
}

/// An allocation: GPU address = CPU pointer, its length, and where it came from (for `Heap::free`).
#[derive(Clone, Copy, Debug)]
pub struct Alloc {
	pub ptr: *mut u8,
	pub len: usize,
	off: usize,
	/// The range handed out may start before `ptr` (alignment padding): this is what goes back to the free list.
	span: (usize, usize),
}

// SAFETY: an allocation is a range of process-lifetime memory; who writes to it is the caller's business (as with wgpu-hal
// buffers in general).
unsafe impl Send for Alloc {}
unsafe impl Sync for Alloc {}

impl Alloc {
	pub fn addr(&self) -> u64 {
		self.ptr as u64
	}

	/// The allocation as words (for descriptor writes). `len` must be a multiple of 4.
	///
	/// # Safety
	/// Nobody else may write the range at the same time.
	#[allow(clippy::mut_from_ref)]
	pub unsafe fn words(&self) -> &mut [u32] {
		// SAFETY: inside the heap's mapping, 4-byte aligned (every alignment used is at least 4).
		unsafe { core::slice::from_raw_parts_mut(self.ptr as *mut u32, self.len / 4) }
	}

	/// Byte offset inside its heap.
	pub fn offset(&self) -> usize {
		self.off
	}
}

impl Heap {
	pub fn new(name: &'static str, len: usize, low: bool) -> Result<Heap, String> {
		let block = plat::map(len, low).map_err(|e| format!("{name} heap ({} MB): {e}", len >> 20))?;
		if low && !plat::FAKE && block.base as u64 + block.len as u64 > 1 << 32 {
			return Err(format!("{name} heap mapped at {:p}, not below 4 GB", block.base));
		}
		let len = block.len;
		Ok(Heap { name, block, free: Mutex::new(BTreeMap::from([(0, len)])), used: AtomicUsize::new(0), peak: AtomicUsize::new(0) })
	}

	/// A heap of CPU-cached memory (`plat::map_cached`) for buffers the CPU reads back.
	pub fn new_cached(name: &'static str, len: usize) -> Result<Heap, String> {
		let block = plat::map_cached(len).map_err(|e| format!("{name} heap ({} MB): {e}", len >> 20))?;
		let len = block.len;
		Ok(Heap { name, block, free: Mutex::new(BTreeMap::from([(0, len)])), used: AtomicUsize::new(0), peak: AtomicUsize::new(0) })
	}

	pub fn base(&self) -> u64 {
		self.block.base as u64
	}

	/// Whether `a` came from this heap.
	pub fn owns(&self, a: &Alloc) -> bool {
		let (b, p) = (self.block.base as usize, a.ptr as usize);
		p >= b && p < b + self.block.len
	}

	/// `len` bytes aligned to `align` (a power of two), or `None` when the heap is full.
	pub fn alloc(&self, len: usize, align: usize) -> Option<Alloc> {
		let len = len.max(4).next_multiple_of(4);
		let base = self.block.base as usize;
		let mut free = self.free.lock();
		let mut found = None;
		for (&start, &flen) in free.iter() {
			let at = (base + start).next_multiple_of(align) - base;
			if at + len > start + flen {
				continue;
			}
			found = Some((start, flen, at));
			break;
		}
		if let Some((start, flen, at)) = found {
			// Hand out [start, at + len): the padding before `at` goes with it and comes back on free.
			let used = at + len - start;
			free.remove(&start);
			if used < flen {
				free.insert(start + used, flen - used);
			}
			// SAFETY: inside the mapping.
			let ptr = unsafe { self.block.base.add(at) };
			drop(free);
			self.note_use(used);
			return Some(Alloc { ptr, len, off: at, span: (start, used) });
		}
		None
	}

	/// Returns an allocation of this heap.
	pub fn free(&self, a: Alloc) {
		let (start, len) = a.span;
		self.used.fetch_sub(len, Relaxed);
		let mut free = self.free.lock();
		let (mut start, mut len) = (start, len);
		// Merge with the next and the previous range.
		if let Some(next) = free.remove(&(start + len)) {
			len += next;
		}
		if let Some((&ps, &pl)) = free.range(..start).next_back() {
			if ps + pl == start {
				start = ps;
				len += pl;
			}
		}
		free.insert(start, len);
	}

	/// Counts `len` more bytes in use. Each time the high-water mark passes another 64 MB (4 MB for a heap of 256 MB or less),
	/// stderr (the game's run log) gets one line: nothing installs a logger for wgpu-hal's `log` calls.
	fn note_use(&self, len: usize) {
		let now = self.used.fetch_add(len, Relaxed) + len;
		let old = self.peak.fetch_max(now, Relaxed);
		let step = if self.block.len <= 256 << 20 { 22 } else { 26 };
		if now > old && now >> step != old >> step {
			std::eprintln!("GNM: {} heap high-water {} MB of {} MB", self.name, now >> 20, self.block.len >> 20);
		}
	}

	/// Bytes in use now.
	pub fn used_bytes(&self) -> usize {
		self.used.load(Relaxed)
	}

	/// The most bytes ever in use.
	pub fn peak_bytes(&self) -> usize {
		self.peak.load(Relaxed)
	}

	/// Bytes still free (for logs).
	pub fn free_bytes(&self) -> usize {
		self.free.lock().iter().map(|r| r.1).sum()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn alloc_free_merge() {
		let h = Heap::new("test", 1 << 20, false).unwrap();
		let a = h.alloc(100, 256).unwrap();
		let b = h.alloc(1000, 4096).unwrap();
		assert_eq!(a.addr() % 256, 0);
		assert_eq!(b.addr() % 4096, 0);
		let c = h.alloc(10, 4).unwrap();
		h.free(b);
		h.free(a);
		h.free(c);
		assert_eq!(h.free_bytes(), 1 << 20);
		assert_eq!(h.free.lock().len(), 1);
	}
}
