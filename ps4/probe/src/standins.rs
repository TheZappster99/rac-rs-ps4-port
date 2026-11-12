//! Stand-ins for the two rac-rs modules that rac-rs/src/gsfull reaches into, so the GS-full source compiles into the probe
//! unchanged: the disc pick (`retail::disc`, used by the widescreen hands, which the probe does not register) and the texture
//! name discovery (`texpack::discover`, a PC dev tool behind RAC_TEXNAMES: always off here).

pub mod retail {
	pub mod disc {
		fn usa() -> bool {
			ee_oracle::ver::disc() == ee_oracle::ver::Disc::Usa
		}

		/// The PAL or USA copy of a table (rac-rs retail/disc.rs).
		pub fn pick<T: ?Sized>(pal: &'static T, usa_twin: &'static T) -> &'static T {
			if usa() { usa_twin } else { pal }
		}

		/// rac-rs maps PAL hands to their USA twins with its generated disc_hands.rs; the probe registers none of these (the
		/// widescreen hands), so on USA there are none.
		pub fn hands(set: &'static [ee_oracle::rec::Hand]) -> &'static [ee_oracle::rec::Hand] {
			if usa() { &[] } else { set }
		}
	}
}

pub mod texpack {
	pub mod discover {
		pub enum Origin {
			#[allow(dead_code)]
			Cap,
			Full,
		}

		#[allow(dead_code)]
		pub struct Draw {
			pub tex0: u64,
			pub clamp: u64,
			pub texa: u64,
			pub texclut: u64,
			pub tex1: u64,
			pub mip1: u64,
			pub mip2: u64,
			pub w: u32,
			pub h: u32,
			pub field: u32,
		}

		pub struct Probe;

		pub fn on() -> Option<&'static std::path::PathBuf> {
			None
		}

		pub fn probe(_: Origin, _: u32, _: Draw) -> Option<Probe> {
			None
		}

		pub fn record(_: Probe, _: &[u8]) {}
	}
}

// Since upstream v0.5.0 the GS-full source also reaches into these (R&C2's HUD overlay pipe `ovl_pipe` and VRAM texture
// requests `texreq`); the probe runs neither, so empty stand-ins are enough.
pub mod level {
	#[allow(dead_code)]
	pub struct Texture {
		pub w: usize,
		pub h: usize,
		pub rgba: Vec<[u8; 4]>,
	}
}

pub mod render {
	#[allow(dead_code)]
	pub struct Image {
		pub w: usize,
		pub h: usize,
		pub rgb: Vec<u8>,
	}

	pub fn png(_: &Image) -> Vec<u8> {
		Vec::new()
	}
}

#[allow(dead_code)]
pub mod gscap {
	pub struct Hd;

	pub struct Gs;

	impl Gs {
		pub fn new(_: Vec<u8>) -> Gs {
			Gs
		}
		pub fn rc2_live(&self) -> bool {
			false
		}
		pub fn rc2_hd_ready(&self, _: u32) -> bool {
			false
		}
		pub fn rc2_field(&mut self, _: &crate::gsfull::overlay::OvlField, _: bool, _: u32, _: bool) -> Option<Hd> {
			None
		}
	}

	pub fn hd_expect_rows(_: u32) {}
}
