//! A census of what the game asks of the PS2's GS each field, to size the PS4 GPU renderer (PS4_PORT_PLAN.md, step 2 part 4):
//! primitive types, how often the draw state changes, blend equations, texture formats and functions, alpha / depth tests,
//! frame and depth buffer formats, uploads, and draws that sample a buffer drawn earlier in the same field (render to
//! texture). It reads the same GS-bound qwords GS-full rasterises (one `Field` from `gsfull::pipe::Pipe::process`) with a
//! small GIF tag parser of its own; nothing in rac-rs changes. PC only (PROBE_CENSUS=FROM:TO fields), written to census.txt.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

/// The draw state that matters to a GPU renderer: one pipeline / shader variant per distinct value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Key {
	pub prim: u8,
	pub tme: bool,
	pub abe: bool,
	pub fge: bool,
	pub iip: bool,
	pub fst: bool,
	/// TEX0: PSM, TFX, TCC (when TME).
	pub tpsm: u8,
	pub tfx: u8,
	pub tcc: bool,
	pub bilinear: bool,
	/// ALPHA register (A, B, C, D, FIX) when ABE.
	pub alpha: u64,
	/// TEST: alpha test enable / method / ref / fail, destination alpha test, depth test enable / method.
	pub test: u64,
	pub fpsm: u8,
	pub zpsm: u8,
	pub zwrite: bool,
	pub fbmsk_any: bool,
	pub fba: bool,
	pub clamp_mode: u8,
}

#[derive(Default)]
struct Path {
	nloop: u32,
	nreg: u32,
	regs: u64,
	flg: u32,
	j: u32,
	eop_seen: bool,
}

#[derive(Default)]
pub struct Census {
	r: Vec<u64>,
	paths: [Path; 4],
	nverts: u32,
	/// Over the fields counted.
	pub fields: u64,
	vertices: u64,
	prims: [u64; 8],
	by_key: HashMap<Key, (u64, u64)>,
	state_changes: u64,
	last_key: Option<Key>,
	uploads_qw: u64,
	uploads: u64,
	local_copies: u64,
	clut_loads: u64,
	/// Draws whose texture base is a frame buffer drawn earlier in the field (render to texture), and of those, the ones
	/// that sample the frame they draw into (feedback).
	rtt_prims: u64,
	feedback_prims: u64,
	rtt_targets: BTreeMap<(u32, u8), u64>,
	frames: BTreeMap<(u32, u32, u8), u64>,
	drawn_fbp: HashMap<u32, ()>,
	per_field_changes: Vec<u64>,
	per_field_prims: Vec<u64>,
	field_changes: u64,
	field_prims: u64,
}

impl Census {
	pub fn field(&mut self, qs: &[u128], segs: &[(u32, u8, u8)]) {
		if self.r.is_empty() {
			self.r = vec![0; 0x80];
		}
		self.drawn_fbp.clear();
		self.field_changes = 0;
		self.field_prims = 0;
		for (i, &(start, path, _)) in segs.iter().enumerate() {
			let end = segs.get(i + 1).map_or(qs.len(), |s| s.0 as usize);
			let p = (path & 3) as usize;
			for &q in &qs[start as usize..end] {
				self.qword(p, q);
			}
		}
		self.fields += 1;
		self.per_field_changes.push(self.field_changes);
		self.per_field_prims.push(self.field_prims);
	}

	fn qword(&mut self, p: usize, q: u128) {
		let lo = q as u64;
		let hi = (q >> 64) as u64;
		if self.paths[p].nloop == 0 {
			// A GIF tag.
			let t = &mut self.paths[p];
			t.nloop = (lo & 0x7fff) as u32;
			t.flg = ((lo >> 58) & 3) as u32;
			t.nreg = match ((lo >> 60) & 0xf) as u32 {
				0 => 16,
				n => n,
			};
			t.regs = hi;
			t.j = 0;
			t.eop_seen = lo & 0x8000 != 0;
			let image = t.flg == 2 && t.nloop > 0;
			if image {
				self.uploads += 1;
			}
			if lo & (1 << 46) != 0 {
				self.write(0x00, (lo >> 47) & 0x7ff);
			}
			return;
		}
		match self.paths[p].flg {
			0 => {
				let t = &self.paths[p];
				let d = ((t.regs >> (4 * t.j)) & 0xf) as u8;
				self.packed(d, q);
				self.step(p);
			}
			1 => {
				// REGLIST: two 64-bit registers a qword.
				for w in [lo, hi] {
					if self.paths[p].nloop == 0 {
						break;
					}
					let t = &self.paths[p];
					let d = ((t.regs >> (4 * t.j)) & 0xf) as u8;
					if d < 0xe {
						self.write(d, w);
					}
					self.step(p);
				}
			}
			_ => {
				self.uploads_qw += 1;
				self.paths[p].nloop -= 1;
			}
		}
	}

	fn step(&mut self, p: usize) {
		let t = &mut self.paths[p];
		t.j += 1;
		if t.j >= t.nreg {
			t.j = 0;
			t.nloop -= 1;
		}
	}

	fn packed(&mut self, d: u8, q: u128) {
		let lo = q as u64;
		match d {
			0x0 => self.write(0x00, lo & 0x7ff),
			0x4 | 0x5 => self.kick(true),
			0xc | 0xd => self.kick(false),
			0x6..=0x9 => self.write(d, lo),
			0xe => self.write(((q >> 64) & 0xff) as u8, lo),
			_ => {}
		}
	}

	fn write(&mut self, a: u8, v: u64) {
		let a = a as usize & 0x7f;
		match a {
			0x00 => {
				self.nverts = 0;
			}
			0x04 | 0x05 => {
				self.r[a] = v;
				self.kick(true);
				return;
			}
			0x0c | 0x0d => {
				self.kick(false);
				return;
			}
			0x53 if v & 3 == 2 => self.local_copies += 1,
			_ => {}
		}
		if (a == 0x06 || a == 0x07) && (v >> 61) & 7 != 0 {
			self.clut_loads += 1;
		}
		self.r[a] = v;
	}

	/// A vertex: counts a primitive when the vertex queue completes one (PRIM type rules) and the kick draws.
	fn kick(&mut self, draw: bool) {
		self.vertices += 1;
		let prim = self.r[0] & 7;
		self.nverts += 1;
		let need = match prim {
			0 => 1,
			1 | 2 | 6 => 2,
			3..=5 => 3,
			_ => return,
		};
		if self.nverts < need {
			return;
		}
		match prim {
			0 | 1 | 3 | 6 => self.nverts = 0,
			_ => self.nverts = need - 1,
		}
		if draw {
			self.draw(prim as u8);
		}
	}

	fn draw(&mut self, prim: u8) {
		// PRMODECONT selects PRIM or PRMODE for the attributes.
		let at = if self.r[0x1a] & 1 != 0 { self.r[0x00] } else { self.r[0x1b] };
		let c = ((at >> 9) & 1) as usize;
		let tme = at & 0x10 != 0;
		let abe = at & 0x40 != 0;
		let tex0 = self.r[0x06 + c];
		let frame = self.r[0x4c + c];
		let zbuf = self.r[0x4e + c];
		let test = self.r[0x47 + c];
		let fbp = ((frame & 0x1ff) * 32) as u32;
		let fpsm = ((frame >> 24) & 0x3f) as u8;
		let tbp = (tex0 & 0x3fff) as u32;
		let k = Key {
			prim,
			tme,
			abe,
			fge: at & 0x20 != 0,
			iip: at & 8 != 0,
			fst: at & 0x100 != 0,
			tpsm: if tme { ((tex0 >> 20) & 0x3f) as u8 } else { 0xff },
			tfx: if tme { ((tex0 >> 35) & 3) as u8 } else { 0 },
			tcc: tme && (tex0 >> 34) & 1 != 0,
			bilinear: tme && self.r[0x14 + c] & 0x20 != 0,
			alpha: if abe { self.r[0x42 + c] & 0xff_0000_00ff } else { 0 },
			// ATE ATST AREF AFAIL DATE DATM ZTE ZTST (bits 0..18), AREF only when the alpha test is on.
			test: if test & 1 != 0 { test & 0x7_ffff } else { test & 0x7_c000 },
			fpsm,
			zpsm: if test & 0x10000 != 0 { ((zbuf >> 24) & 0xf) as u8 } else { 0xff },
			zwrite: test & 0x10000 != 0 && (zbuf >> 32) & 1 == 0,
			fbmsk_any: (frame >> 32) as u32 != 0,
			fba: self.r[0x4a + c] & 1 != 0,
			clamp_mode: if tme { (self.r[0x08 + c] & 0xf) as u8 } else { 0 },
		};
		self.prims[prim as usize] += 1;
		self.field_prims += 1;
		let e = self.by_key.entry(k).or_insert((0, 0));
		e.0 += 1;
		if self.last_key != Some(k) {
			e.1 += 1;
			self.state_changes += 1;
			self.field_changes += 1;
			self.last_key = Some(k);
		}
		*self.frames.entry((fbp, ((frame >> 16) & 0x3f) as u32, fpsm)).or_insert(0) += 1;
		if tme && self.drawn_fbp.contains_key(&tbp) {
			self.rtt_prims += 1;
			*self.rtt_targets.entry((tbp, k.tpsm)).or_insert(0) += 1;
			if tbp == fbp {
				self.feedback_prims += 1;
			}
		}
		self.drawn_fbp.insert(fbp, ());
	}

	pub fn report(&self) -> String {
		let f = self.fields.max(1) as f64;
		let mut s = String::new();
		let _ = writeln!(s, "GS census over {} fields", self.fields);
		let _ = writeln!(s, "per field: {:.0} vertices, {:.0} prims, {:.0} draw state changes, {:.1} uploads ({:.0} qwords), {:.1} local copies, {:.1} CLUT loads",
			self.vertices as f64 / f, self.prims.iter().sum::<u64>() as f64 / f, self.state_changes as f64 / f, self.uploads as f64 / f, self.uploads_qw as f64 / f, self.local_copies as f64 / f, self.clut_loads as f64 / f);
		let max = |v: &Vec<u64>| v.iter().copied().max().unwrap_or(0);
		let _ = writeln!(s, "worst field: {} prims, {} state changes", max(&self.per_field_prims), max(&self.per_field_changes));
		let names = ["point", "line", "line strip", "triangle", "tri strip", "tri fan", "sprite", "?"];
		let _ = writeln!(s, "prims by type (per field): {}", (0..7).filter(|&i| self.prims[i] > 0).map(|i| format!("{} {:.0}", names[i], self.prims[i] as f64 / f)).collect::<Vec<_>>().join(", "));
		let _ = writeln!(s, "render to texture: {:.1} prims a field sample a buffer drawn earlier in the field ({:.1} sample their own frame)", self.rtt_prims as f64 / f, self.feedback_prims as f64 / f);
		for ((tbp, psm), n) in &self.rtt_targets {
			let _ = writeln!(s, "  texture at block {tbp:#x} psm {psm:#x}: {:.1} prims a field", *n as f64 / f);
		}
		let _ = writeln!(s, "frame buffers drawn (fbp blocks, fbw, psm): prims a field");
		for ((fbp, fbw, psm), n) in &self.frames {
			let _ = writeln!(s, "  {fbp:#06x} w{fbw} psm {psm:#x}: {:.1}", *n as f64 / f);
		}
		// Distinct values of each part of the key, weighted by prims.
		let mut part = |name: &str, g: &dyn Fn(&Key) -> String| {
			let mut m: BTreeMap<String, u64> = BTreeMap::new();
			for (k, (n, _)) in &self.by_key {
				*m.entry(g(k)).or_insert(0) += n;
			}
			let mut v: Vec<_> = m.into_iter().collect();
			v.sort_by(|a, b| b.1.cmp(&a.1));
			let _ = writeln!(s, "{name}: {}", v.iter().map(|(k, n)| format!("{k} {:.0}", *n as f64 / f)).collect::<Vec<_>>().join(", "));
		};
		part("texture psm (ff = untextured)", &|k| format!("{:#x}", k.tpsm));
		part("tfx/tcc", &|k| if k.tme { format!("tfx{} tcc{}", k.tfx, k.tcc as u8) } else { "-".into() });
		part("alpha blend ABCD (0 Cs 1 Cd 2 0 / C: 0 As 1 Ad 2 FIX)", &|k| {
			if !k.abe {
				return "off".into();
			}
			let a = k.alpha;
			format!("{}{}{}{} fix{}", a & 3, (a >> 2) & 3, (a >> 4) & 3, (a >> 6) & 3, (a >> 32) & 0xff)
		});
		part("alpha test (ATE ATST AREF AFAIL)", &|k| if k.test & 1 != 0 { format!("atst{} aref{} afail{}", (k.test >> 1) & 7, (k.test >> 4) & 0xff, (k.test >> 12) & 3) } else { "off".into() });
		part("dest alpha test", &|k| if k.test & 0x4000 != 0 { format!("datm{}", (k.test >> 15) & 1) } else { "off".into() });
		part("depth (ZTE ZTST zpsm zwrite)", &|k| if k.test & 0x10000 != 0 { format!("ztst{} zpsm{} zw{}", (k.test >> 17) & 3, k.zpsm, k.zwrite as u8) } else { "off".into() });
		part("frame psm", &|k| format!("{:#x}", k.fpsm));
		part("fog / gouraud / fst / bilinear / fbmask / fba", &|k| format!("fge{} iip{} fst{} bil{} fbm{} fba{}", k.fge as u8, k.iip as u8, k.fst as u8, k.bilinear as u8, k.fbmsk_any as u8, k.fba as u8));
		part("clamp mode (wms wmt)", &|k| format!("{:#x}", k.clamp_mode));
		let _ = writeln!(s, "distinct draw states: {}", self.by_key.len());
		let mut v: Vec<_> = self.by_key.iter().collect();
		v.sort_by(|a, b| b.1.0.cmp(&a.1.0));
		let _ = writeln!(s, "top states (prims a field, entries a field):");
		for (k, (n, e)) in v.iter().take(40) {
			let _ = writeln!(s, "  {:7.1} {:6.1}  {k:?}", *n as f64 / f, *e as f64 / f);
		}
		s
	}
}
