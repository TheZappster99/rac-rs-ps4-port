//! PS4 port: the console's own controller (DualShock 4) through `scePad`.
//!
//! Like `pspad` (Windows, HID) and `lxpad` (Linux, evdev), this ends in an `XRaw`, the XInput report, so everything downstream (the
//! button mapping and remapping, the deadzones, the button prompts) is shared and does not know the pad came from a PS4. `retail.rs`
//! names it `hidpad` on the PS4 build.
//!
//! The pad belongs to the user who started the app (`sceUserServiceGetInitialUser`). Opening can fail while the system is still
//! starting the app or when no controller is signed in, so a failed open is retried every second instead of giving up. `state()` reads
//! the pad's current state (`scePadReadState`, a cheap call) each time it is asked; the game asks a few times per field.
//!
//! Only the PS4 build talks to the console (`sys`); the report maths (`to_xraw`) is plain and compiled for the unit tests everywhere.

use super::prompts::XRaw;

/// scePad's button bits (OrbisPadButton) and the XInput bit each becomes: Cross = A, Circle = B, Square = X, Triangle = Y, L1 = LB,
/// R1 = RB, Options = Start, the touchpad click = Back (the PS2's Select: the PS4 keeps the Share button for itself), L3, R3, the
/// d-pad. L2 / R2 come from the analog triggers (as on the PC paths), not from their bits.
const MAP: [(u32, u16); 13] = [
	(0x4000, 0x1000),   // Cross -> A
	(0x2000, 0x2000),   // Circle -> B
	(0x8000, 0x4000),   // Square -> X
	(0x1000, 0x8000),   // Triangle -> Y
	(0x0400, 0x0100),   // L1 -> LB
	(0x0800, 0x0200),   // R1 -> RB
	(0x0008, 0x0010),   // Options -> Start
	(0x100000, 0x0020), // touchpad -> Back
	(0x0002, 0x0040),   // L3
	(0x0004, 0x0080),   // R3
	(0x0010, 0x0001),   // up
	(0x0040, 0x0002),   // down
	(0x0080, 0x0004),   // left
];
/// Right is the one d-pad bit left over (MAP holds 13 entries to keep the table readable).
const RIGHT: (u32, u16) = (0x0020, 0x0008);

/// A stick axis (0..=255, 0x80 centre, as on the PS2) in XInput's range; `up` flips it (XInput's y is positive up, the pad's grows down).
/// The same scale as `pspad::axis`, so a stick reads the same on every platform.
fn axis(v: u8, up: bool) -> i16 {
	let d = v as i32 - 0x80;
	((if up { -d } else { d }) * 258).clamp(-32768, 32767) as i16
}

/// One `scePadReadState` result as an XInput report.
pub fn to_xraw(buttons: u32, left: [u8; 2], right: [u8; 2], l2: u8, r2: u8) -> XRaw {
	let mut b = 0u16;
	for &(from, to) in MAP.iter().chain(std::iter::once(&RIGHT)) {
		if buttons & from != 0 {
			b |= to;
		}
	}
	XRaw { buttons: b, lt: l2, rt: r2, lx: axis(left[0], false), ly: axis(left[1], true), rx: axis(right[0], false), ry: axis(right[1], true) }
}

#[cfg(target_os = "freebsd")]
pub use sys::{connected, state};

/// Not the PS4: no pad here (the module is only named `hidpad` on the PS4; this keeps it compiling for the tests).
#[cfg(not(target_os = "freebsd"))]
#[allow(dead_code)]
pub fn state() -> Option<XRaw> {
	None
}
#[cfg(not(target_os = "freebsd"))]
#[allow(dead_code)]
pub fn connected() -> bool {
	false
}

#[cfg(target_os = "freebsd")]
mod sys {
	use super::XRaw;
	use std::sync::Mutex;
	use std::time::{Duration, Instant};

	/// OrbisPadData (OpenOrbis `_types/pad.h`, 0x78 bytes). Only the first fields are read; the rest is there for the size.
	#[repr(C)]
	struct PadData {
		buttons: u32,
		left: [u8; 2],
		right: [u8; 2],
		l2: u8,
		r2: u8,
		_pad: u16,
		_quat: [f32; 4],
		_vel: [f32; 3],
		_acc: [f32; 3],
		_touch: [u8; 24],
		connected: u8,
		_timestamp: u64,
		_ext: [u8; 16],
		_count: u8,
		_unknown: [u8; 15],
		/// Room in case the console's struct is larger than OpenOrbis's reading of it.
		_spare: [u8; 64],
	}

	unsafe extern "C" {
		fn sceUserServiceInitialize(params: *const core::ffi::c_void) -> i32;
		fn sceUserServiceGetInitialUser(user: *mut i32) -> i32;
		fn scePadInit() -> i32;
		fn scePadOpen(user: i32, kind: i32, index: i32, param: *const core::ffi::c_void) -> i32;
		fn scePadReadState(handle: i32, data: *mut PadData) -> i32;
	}

	struct Pad {
		/// The open pad handle, or None while it could not be opened.
		handle: Option<i32>,
		/// When the next open is tried (None: at once).
		retry: Option<Instant>,
		/// The user the pad belongs to (None before the user service gave one).
		user: Option<i32>,
		/// Whether the services were initialised.
		init: bool,
		/// Whether the last read saw a connected controller.
		connected: bool,
		/// The last read failure logged (to log each new one once).
		last_err: i32,
	}

	static PAD: Mutex<Pad> = Mutex::new(Pad { handle: None, retry: None, user: None, init: false, connected: false, last_err: 0 });

	impl Pad {
		/// Opens the pad when it is not open and the retry time has come.
		fn open(&mut self) {
			if self.handle.is_some() {
				return;
			}
			let now = Instant::now();
			if self.retry.is_some_and(|t| now < t) {
				return;
			}
			self.retry = Some(now + Duration::from_secs(1));
			// SAFETY: plain calls into the system libraries; the user service takes a null parameter block (default settings).
			unsafe {
				if !self.init {
					self.init = true;
					let r = sceUserServiceInitialize(std::ptr::null());
					let p = scePadInit();
					eprintln!("ps4pad: user service init {r:#x}, pad init {p:#x}");
				}
				if self.user.is_none() {
					let mut u = -1;
					let r = sceUserServiceGetInitialUser(&mut u);
					if r == 0 && u >= 0 {
						eprintln!("ps4pad: initial user {u:#x}");
						self.user = Some(u);
					} else if r != self.last_err {
						self.last_err = r;
						eprintln!("ps4pad: no initial user yet ({r:#x}), retrying every second");
						return;
					} else {
						return;
					}
				}
				let h = scePadOpen(self.user.unwrap_or(0), 0, 0, std::ptr::null());
				if h >= 0 {
					eprintln!("ps4pad: controller opened (handle {h})");
					self.handle = Some(h);
					self.last_err = 0;
				} else if h != self.last_err {
					self.last_err = h;
					eprintln!("ps4pad: could not open the controller ({h:#x}), retrying every second");
				}
			}
		}

		fn read(&mut self) -> Option<XRaw> {
			self.open();
			let h = self.handle?;
			// SAFETY: `PadData` is at least as large as the console's struct; zeroed is a valid value for it.
			let mut d: PadData = unsafe { std::mem::zeroed() };
			let r = unsafe { scePadReadState(h, &mut d) };
			if r != 0 {
				if r != self.last_err {
					self.last_err = r;
					eprintln!("ps4pad: read failed ({r:#x})");
				}
				self.connected = false;
				return None;
			}
			let was = std::mem::replace(&mut self.connected, d.connected != 0);
			if was != self.connected {
				eprintln!("ps4pad: controller {}", if self.connected { "connected" } else { "disconnected" });
			}
			if !self.connected {
				return None;
			}
			Some(super::to_xraw(d.buttons, d.left, d.right, d.l2, d.r2))
		}
	}

	/// The controller's current state; None without one.
	pub fn state() -> Option<XRaw> {
		PAD.lock().ok()?.read()
	}

	/// Whether a controller is open and connected.
	pub fn connected() -> bool {
		let Ok(mut p) = PAD.lock() else { return false };
		if p.handle.is_none() {
			p.open();
		}
		p.handle.is_some() && (p.connected || p.read().is_some())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ps4_pad_maps_like_the_pc_paths() {
		// Cross + Options + right on the d-pad, sticks centred, no triggers.
		let r = to_xraw(0x4000 | 0x0008 | 0x0020, [0x80, 0x80], [0x80, 0x80], 0, 0);
		assert_eq!(r.buttons, 0x1000 | 0x0010 | 0x0008);
		assert_eq!((r.lx, r.ly, r.rx, r.ry), (0, 0, 0, 0));
		// Left stick fully up reads positive y (XInput), right stick fully right positive x; the triggers pass through.
		let r = to_xraw(0, [0x80, 0x00], [0xff, 0x80], 200, 10);
		assert!(r.ly > 32000 && r.rx > 32000);
		assert_eq!((r.lt, r.rt), (200, 10));
		// The touchpad is the PS2's Select (XInput Back).
		assert_eq!(to_xraw(0x100000, [0x80; 2], [0x80; 2], 0, 0).buttons, 0x0020);
	}
}
