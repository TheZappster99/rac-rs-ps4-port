//! The TV: the PS4's video out (libSceVideoOut) with CPU-filled frame buffers in direct memory. A presenter thread shows colour
//! bars until the first picture of the game is ready, then each new picture of the GS-full pipeline (gsf.rs), stretched to 4:3
//! in the middle of a 1920x1080 buffer. No GPU work at all: the PS2 picture is small (512x448) and the copy runs on a core the
//! game does not use.
//!
//! On the PC there is no TV: `start` does nothing (main.rs writes PPM snapshots instead).

use crate::gsf::Shared;
use std::sync::Arc;

#[cfg(target_os = "freebsd")]
mod ps4 {
	use core::ffi::{c_char, c_void};

	#[repr(C)]
	#[derive(Default)]
	pub struct BufferAttribute {
		pub format: i32,
		pub tmode: i32,
		pub aspect: i32,
		pub width: u32,
		pub height: u32,
		pub pitch: u32,
		pub reserved: [u64; 2],
	}

	#[repr(C)]
	#[derive(Default)]
	pub struct ResolutionStatus {
		pub width: u32,
		pub height: u32,
		pub pane_width: u32,
		pub pane_height: u32,
		pub refresh_rate: u64,
		pub screen_size: f32,
		pub flags: u16,
		pub reserved0: u16,
		pub reserved1: [u32; 3],
	}

	unsafe extern "C" {
		pub fn sceVideoOutOpen(user: i32, bus: i32, index: i32, param: *const c_void) -> i32;
		pub fn sceVideoOutSetBufferAttribute(attr: *mut BufferAttribute, format: u32, tmode: u32, aspect: u32, w: u32, h: u32, pitch: u32);
		pub fn sceVideoOutRegisterBuffers(h: i32, start: i32, addrs: *const *mut c_void, n: i32, attr: *const BufferAttribute) -> i32;
		pub fn sceVideoOutSubmitFlip(h: i32, idx: i32, mode: u32, arg: i64) -> i32;
		pub fn sceVideoOutSetFlipRate(h: i32, rate: i32) -> i32;
		pub fn sceVideoOutAddFlipEvent(eq: u64, h: i32, udata: *mut c_void) -> i32;
		pub fn sceVideoOutGetResolutionStatus(h: i32, st: *mut ResolutionStatus) -> i32;
		pub fn sceKernelCreateEqueue(eq: *mut u64, name: *const c_char) -> i32;
		/// The event is FreeBSD's `struct kevent` (32 bytes).
		pub fn sceKernelWaitEqueue(eq: u64, ev: *mut [u64; 4], n: i32, out: *mut i32, timeout: *mut u32) -> i32;
		pub fn sceKernelGetDirectMemorySize() -> usize;
		pub fn sceKernelAllocateDirectMemory(start: i64, end: i64, len: usize, align: usize, mem_type: i32, phys: *mut i64) -> i32;
		pub fn sceKernelMapDirectMemory(addr: *mut *mut c_void, len: usize, prot: i32, flags: i32, phys: i64, align: usize) -> i32;
	}

	/// The main user's screen (ORBIS_VIDEO_USER_MAIN, bus main).
	pub const USER_MAIN: i32 = 0xff;
	/// 8-bit A8R8G8B8 with sRGB output (a u32 0xAARRGGBB per pixel), linear (untiled) layout.
	pub const FORMAT_A8R8G8B8_SRGB: u32 = 0x8000_0000;
	pub const TILING_LINEAR: u32 = 1;
	pub const FLIP_VSYNC: u32 = 1;
	/// Write-combined memory (CPU writes stream straight to RAM; the display engine reads it).
	pub const MEM_WC_GARLIC: i32 = 3;
	/// CPU read + write, GPU read + write.
	pub const PROT_ALL: i32 = 0x33;
}

/// The buffer the TV gets: 1080p (the console scales it to the TV's mode).
const W: usize = 1920;
const H: usize = 1080;
/// Buffers flipped in turn: one on screen, one queued, one being drawn.
const NBUF: usize = 3;

/// Opens the video out and starts the presenter; logs what it did through `say`.
#[cfg(target_os = "freebsd")]
pub fn start(shared: Arc<Shared>, say: &mut impl FnMut(String)) {
	use ps4::*;
	// SAFETY: plain libSceVideoOut / libkernel calls with valid out-pointers; the frame buffers are a fresh direct-memory mapping
	// that nothing else uses, handed to the presenter thread for good.
	unsafe {
		let h = sceVideoOutOpen(USER_MAIN, 0, 0, core::ptr::null());
		if h < 0 {
			say(format!("vout: sceVideoOutOpen failed {h:#x}"));
			return;
		}
		let mut rs = ResolutionStatus::default();
		let r = sceVideoOutGetResolutionStatus(h, &mut rs);
		say(format!("vout: handle {h}, TV mode {}x{} ({}x{} pane, refresh {:#x}, r={r:#x})", rs.width, rs.height, rs.pane_width, rs.pane_height, rs.refresh_rate));
		let mut eq = 0u64;
		let r = sceKernelCreateEqueue(&mut eq, c"rac-rs flip".as_ptr());
		if r < 0 {
			say(format!("vout: sceKernelCreateEqueue failed {r:#x}"));
			return;
		}
		sceVideoOutAddFlipEvent(eq, h, core::ptr::null_mut());
		// Direct memory, 2 MB aligned (what the display engine wants), all buffers in one block.
		let one = W * H * 4;
		let align = 2 << 20;
		let len = (one * NBUF).div_ceil(align) * align;
		let mut phys = 0i64;
		let r = sceKernelAllocateDirectMemory(0, sceKernelGetDirectMemorySize() as i64, len, align, MEM_WC_GARLIC, &mut phys);
		if r < 0 {
			say(format!("vout: sceKernelAllocateDirectMemory({len}) failed {r:#x}"));
			return;
		}
		let mut base: *mut core::ffi::c_void = core::ptr::null_mut();
		let r = sceKernelMapDirectMemory(&mut base, len, PROT_ALL, 0, phys, align);
		if r < 0 || base.is_null() {
			say(format!("vout: sceKernelMapDirectMemory failed {r:#x}"));
			return;
		}
		let bufs: Vec<*mut core::ffi::c_void> = (0..NBUF).map(|i| (base as *mut u8).add(i * one) as *mut core::ffi::c_void).collect();
		let mut attr = BufferAttribute::default();
		sceVideoOutSetBufferAttribute(&mut attr, FORMAT_A8R8G8B8_SRGB, TILING_LINEAR, 0, W as u32, H as u32, W as u32);
		let r = sceVideoOutRegisterBuffers(h, 0, bufs.as_ptr(), NBUF as i32, &attr);
		if r < 0 {
			say(format!("vout: sceVideoOutRegisterBuffers failed {r:#x}"));
			return;
		}
		sceVideoOutSetFlipRate(h, 0);
		say(format!("vout: {NBUF} buffers of {W}x{H} registered ({} MB of direct memory)", len >> 20));
		let bufs: Vec<usize> = bufs.iter().map(|&p| p as usize).collect();
		let spawned = std::thread::Builder::new().name("vout".into()).stack_size(1 << 20).spawn(move || {
			let bufs: Vec<&mut [u32]> = bufs.iter().map(|&p| std::slice::from_raw_parts_mut(p as *mut u32, W * H)).collect();
			present_loop(bufs, shared, |i, arg| {
				sceVideoOutSubmitFlip(h, i as i32, FLIP_VSYNC, arg);
			}, |flipped| {
				if flipped {
					// Until that flip is done (the next vblank); the timeout keeps a lost event from hanging the presenter.
					let mut ev = [0u64; 4];
					let mut n = 0;
					let mut us = 100_000u32;
					sceKernelWaitEqueue(eq, &mut ev, 1, &mut n, &mut us);
				} else {
					// Nothing new was shown: look for a picture again soon.
					std::thread::sleep(std::time::Duration::from_millis(2));
				}
			});
		});
		if let Err(e) = spawned {
			say(format!("vout: presenter thread failed: {e}"));
		}
	}
}

#[cfg(not(target_os = "freebsd"))]
pub fn start(_shared: Arc<Shared>, say: &mut impl FnMut(String)) {
	say("vout: no TV on this system (PPM snapshots only)".into());
}

/// The presenter: colour bars until the game's first picture, then every new picture. `flip(i, arg)` shows buffer i; `wait(flipped)`
/// blocks until that flip is done (flipped) or for a moment (nothing was flipped).
#[cfg_attr(not(target_os = "freebsd"), allow(dead_code))]
fn present_loop(mut bufs: Vec<&mut [u32]>, shared: Arc<Shared>, mut flip: impl FnMut(usize, i64), mut wait: impl FnMut(bool)) {
	for b in bufs.iter_mut() {
		bars(b);
	}
	let mut cur = 0usize;
	flip(cur, 0);
	let mut shown = 0i64;
	// The 4:3 picture area in the middle of the 16:9 buffer; the sides stay black.
	let (pw, px0) = (H * 4 / 3, (W - H * 4 / 3) / 2);
	let mut xmap: Vec<u32> = Vec::new();
	let mut map_w = 0u32;
	let mut row = vec![0u32; pw];
	let mut cleared = 0usize;
	let mut flipped = true;
	loop {
		wait(flipped);
		flipped = false;
		let Some(f) = shared.slot.lock().ok().and_then(|mut s| s.take()) else { continue };
		if f.w == 0 || f.h == 0 || f.rgba.len() < (f.w * f.h * 4) as usize {
			continue;
		}
		let n = bufs.len();
		cur = (cur + 1) % n;
		let b = &mut *bufs[cur];
		// The first time a buffer gets a game picture, its colour bars go (the side bars become black).
		if cleared < n {
			b.fill(0xff00_0000);
			cleared += 1;
		}
		if map_w != f.w {
			map_w = f.w;
			xmap = (0..pw).map(|x| (x * f.w as usize / pw) as u32).collect();
		}
		// Nearest-neighbour stretch: one source row is converted once into `row`, then copied to every TV row it covers
		// (write-combined memory likes whole-row streaming writes).
		let mut last_sy = usize::MAX;
		for y in 0..H {
			let sy = y * f.h as usize / H;
			if sy != last_sy {
				last_sy = sy;
				let src = &f.rgba[sy * f.w as usize * 4..(sy + 1) * f.w as usize * 4];
				for (d, &sx) in row.iter_mut().zip(&xmap) {
					let p = &src[sx as usize * 4..sx as usize * 4 + 4];
					*d = 0xff00_0000 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32;
				}
			}
			b[y * W + px0..y * W + px0 + pw].copy_from_slice(&row);
		}
		shown += 1;
		flip(cur, shown);
		flipped = true;
	}
}

/// SMPTE-style colour bars: proof that the video out works before the game draws anything.
#[cfg_attr(not(target_os = "freebsd"), allow(dead_code))]
fn bars(b: &mut [u32]) {
	const C: [u32; 8] = [0xffc0c0c0, 0xffc0c000, 0xff00c0c0, 0xff00c000, 0xffc000c0, 0xffc00000, 0xff0000c0, 0xff101010];
	let mut row = vec![0u32; W];
	for (x, p) in row.iter_mut().enumerate() {
		*p = C[x * 8 / W];
	}
	for y in 0..H {
		b[y * W..(y + 1) * W].copy_from_slice(&row);
	}
}
