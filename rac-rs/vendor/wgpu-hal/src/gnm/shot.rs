//! Console diagnostics: pictures of chosen frames, straight from the display buffers. `RAC_PS4_SHOT=600,1800` (env.txt on the
//! console) saves frames 600 and 1800 as `shot_00600.bmp` ... in `RAC_PS4_SHOT_DIR` (default: /data/rac-rs on the console, the
//! working folder on the PC, where the fake GPU's buffers are black). The only way to see what the console draws without a
//! capture card: copy the files back over FTP.

use alloc::{string::String, vec::Vec};
use std::sync::OnceLock;

fn frames() -> &'static [u64] {
	static F: OnceLock<Vec<u64>> = OnceLock::new();
	F.get_or_init(|| std::env::var("RAC_PS4_SHOT").map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect()).unwrap_or_default())
}

/// Whether frame `n` (1 = the first shown) is to be saved.
pub fn wanted(n: u64) -> bool {
	frames().contains(&n)
}

/// Copies the finished frame at `ptr` (BGRA, `pitch` texels a row) and writes it as a 32-bit BMP on a thread of its own, so
/// the game only pays for the copy out of the display buffer.
pub fn write(n: u64, ptr: *const u8, pitch: u32, width: u32, height: u32) {
	let row = width as usize * 4;
	let mut px = Vec::with_capacity(row * height as usize);
	for y in 0..height as usize {
		// SAFETY: the display buffer holds `height` rows of `pitch` texels, and the GPU has finished writing it.
		let src = unsafe { core::slice::from_raw_parts(ptr.add(y * pitch as usize * 4), row) };
		px.extend_from_slice(src);
	}
	let dir: String = std::env::var("RAC_PS4_SHOT_DIR").unwrap_or_else(|_| if cfg!(target_os = "freebsd") { "/data/rac-rs".into() } else { ".".into() });
	let _ = std::thread::Builder::new().name("gnm-shot".into()).stack_size(256 << 10).spawn(move || {
		let path = alloc::format!("{dir}/shot_{n:05}.bmp");
		let mut f = Vec::with_capacity(54 + px.len());
		let size = (54 + px.len()) as u32;
		// BITMAPFILEHEADER + BITMAPINFOHEADER; a negative height stores the rows top first, as the buffer has them.
		f.extend_from_slice(b"BM");
		f.extend_from_slice(&size.to_le_bytes());
		f.extend_from_slice(&[0; 4]);
		f.extend_from_slice(&54u32.to_le_bytes());
		f.extend_from_slice(&40u32.to_le_bytes());
		f.extend_from_slice(&(width as i32).to_le_bytes());
		f.extend_from_slice(&(-(height as i32)).to_le_bytes());
		f.extend_from_slice(&1u16.to_le_bytes());
		f.extend_from_slice(&32u16.to_le_bytes());
		f.extend_from_slice(&[0; 4]);
		f.extend_from_slice(&(px.len() as u32).to_le_bytes());
		f.extend_from_slice(&[0; 16]);
		f.extend_from_slice(&px);
		match std::fs::write(&path, &f) {
			Ok(()) => std::eprintln!("GNM: frame {n} saved as {path}"),
			Err(e) => std::eprintln!("GNM: could not save frame {n} as {path}: {e}"),
		}
	});
}
