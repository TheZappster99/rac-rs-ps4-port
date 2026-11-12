//! shcache CAPTURE CAPBIN CAPRSH OUT_FILE
//!
//! Packs the PS4 shader cache (`rac_ps4_gnm::cache`) from:
//!   CAPTURE  the PC game's records (`RAC_PS4_CAPTURE`): `<key>.rec`;
//!   CAPBIN   `ps4/tools/vkc/vkrec`'s output: `<key>.sets` (radv's set memory layout);
//!   CAPRSH   `ps4/tools/radvbin`'s output: `<key>.<n>.rsh`, one per compiled stage.
//! Each pipeline is checked before it goes in, and the written file is read back and compared. Problems are listed; the
//! exit code is 1 if any pipeline was left out.
//!
//! Checks per pipeline: one .rsh per stage line, each parses and has the stage the record says; every descriptor set a
//! shader reads exists in the record and has its offsets in .sets; every binding of the record has an offset or a dynamic
//! index; for render pipelines every fragment input is a vertex output. Scratch use (register spills) is reported, since the
//! PS4 back end must give those shaders a scratch ring.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rac_ps4_gnm::cache::{self, NewEntry};
use rac_ps4_gnm::shader::{Shader, Stage};

struct Loaded {
	key: u64,
	label: String,
	rec: String,
	sets: String,
	stages: Vec<Vec<u8>>,
	scratch: u32,
}

fn check(dir_bin: &Path, dir_rsh: &Path, rec_path: &Path) -> Result<Loaded, String> {
	let rec = std::fs::read_to_string(rec_path).map_err(|e| format!("cannot read: {e}"))?;
	let field = |name: &str| rec.lines().find_map(|l| l.strip_prefix(name).map(str::trim));
	let key_hex = field("key ").ok_or("no key line")?;
	let key = u64::from_str_radix(key_hex, 16).map_err(|_| "bad key")?;
	let label = field("label ").unwrap_or("-").to_string();
	let sets = std::fs::read_to_string(dir_bin.join(format!("{key_hex}.sets"))).map_err(|e| format!("no .sets ({e}): did vkrec compile it?"))?;

	// The record's sets and bindings, and what radv's layout says about them.
	let mut rec_bindings: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
	let mut cur = None;
	for l in rec.lines() {
		let w: Vec<&str> = l.split_whitespace().collect();
		match w.as_slice() {
			["set", s, ..] => {
				let s: u32 = s.parse().map_err(|_| "bad set line")?;
				rec_bindings.insert(s, Vec::new());
				cur = Some(s);
			}
			["b", b, ..] => {
				let s = cur.ok_or("binding before a set")?;
				rec_bindings.get_mut(&s).unwrap().push(b.parse().map_err(|_| "bad binding line")?);
			}
			_ => {}
		}
	}
	let mut laid: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
	for l in sets.lines() {
		let w: Vec<&str> = l.split_whitespace().collect();
		match w.as_slice() {
			["set", s, _size] => {
				laid.insert(s.parse().map_err(|_| "bad .sets line")?, Vec::new());
			}
			["b" | "dyn", s, b, _] => {
				let s: u32 = s.parse().map_err(|_| "bad .sets line")?;
				laid.entry(s).or_default().push(b.parse().map_err(|_| "bad .sets line")?);
			}
			_ => return Err(format!(".sets: unknown line {l:?}")),
		}
	}
	for (s, bs) in &rec_bindings {
		let got = laid.get(s).ok_or(format!("set {s} has no layout in .sets"))?;
		for b in bs {
			if !got.contains(b) {
				return Err(format!("set {s} binding {b} has no offset in .sets"));
			}
		}
	}

	// Stages.
	let want: Vec<Stage> = rec
		.lines()
		.filter_map(|l| l.strip_prefix("stage "))
		.map(|l| match l.split_whitespace().next() {
			Some("1") => Ok(Stage::Vertex),
			Some("16") => Ok(Stage::Fragment),
			Some("32") => Ok(Stage::Compute),
			s => Err(format!("stage {s:?} is not vertex, fragment or compute")),
		})
		.collect::<Result<_, _>>()?;
	let mut stages = Vec::new();
	for i in 0..want.len() {
		let p = dir_rsh.join(format!("{key_hex}.{i}.rsh"));
		stages.push(std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?);
	}
	if dir_rsh.join(format!("{key_hex}.{}.rsh", want.len())).exists() {
		return Err(format!("more compiled stages than the record's {}", want.len()));
	}
	let parsed: Vec<Shader> = stages.iter().map(|b| Shader::parse(b)).collect::<Result<_, _>>().map_err(|e| format!("bad .rsh: {e:?}"))?;
	let mut scratch = 0;
	for (i, (s, w)) in parsed.iter().zip(&want).enumerate() {
		if s.stage != *w {
			return Err(format!("stage {i} is {:?}, the record says {w:?}", s.stage));
		}
		for set in 0..32 {
			if s.set_sgpr(set).is_some() && !rec_bindings.contains_key(&(set as u32)) {
				return Err(format!("stage {i} reads set {set}, which the record does not have"));
			}
		}
		scratch = scratch.max(s.scratch_bytes_per_wave);
	}
	if let (Some(vs), Some(fs)) = (parsed.iter().find(|s| s.stage == Stage::Vertex), parsed.iter().find(|s| s.stage == Stage::Fragment)) {
		for k in 0..32 {
			// The VS table is indexed by Mesa varying slot; VAR k is slot 32 + k (as `Shader::ps_input_cntl` reads it).
			if fs.ps_input_mask & (1 << k) != 0 && vs.vs_param_offset[32 + k] == 0xff {
				return Err(format!("fragment input {k} is not written by the vertex stage"));
			}
		}
	}
	Ok(Loaded { key, label, rec, sets, stages, scratch })
}

fn main() {
	let a: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
	let [cap, bin, rsh, out] = a.as_slice() else {
		eprintln!("usage: shcache CAPTURE CAPBIN CAPRSH OUT_FILE");
		std::process::exit(2);
	};
	let mut recs: Vec<PathBuf> = std::fs::read_dir(cap)
		.expect("capture folder")
		.filter_map(|e| e.ok().map(|e| e.path()))
		.filter(|p| p.extension().is_some_and(|x| x == "rec"))
		.collect();
	recs.sort();
	let mut ok = Vec::new();
	let mut problems = 0;
	for p in &recs {
		match check(bin, rsh, p) {
			Ok(l) => ok.push(l),
			Err(e) => {
				problems += 1;
				println!("LEFT OUT {}: {e}", p.file_name().unwrap().to_string_lossy());
			}
		}
	}
	for l in ok.iter().filter(|l| l.scratch > 0) {
		println!("scratch: {:016x} ({}) needs {} bytes per wave", l.key, l.label, l.scratch);
	}
	let mut entries: Vec<NewEntry> = ok
		.iter()
		.map(|l| NewEntry { key: l.key, rec: &l.rec, sets: &l.sets, stages: l.stages.iter().map(Vec::as_slice).collect() })
		.collect();
	let file = cache::write(&mut entries);
	// Read it back: every pipeline must come out exactly as it went in.
	let c = cache::Cache::parse(&file).expect("the written cache parses");
	assert_eq!(c.len(), ok.len());
	for l in &ok {
		let e = c.get(l.key).expect("entry parses").expect("entry present");
		assert!(e.rec == l.rec && e.sets == l.sets && e.stages.len() == l.stages.len());
		assert!(e.stages.iter().zip(&l.stages).all(|(a, b)| *a == b.as_slice()));
	}
	std::fs::write(out, &file).expect("write the cache");
	let render = ok.iter().filter(|l| l.rec.contains("\nkind render\n")).count();
	println!(
		"shcache: {} pipelines ({render} render, {} compute), {} left out, {} KB -> {}",
		ok.len(),
		ok.len() - render,
		problems,
		file.len() / 1024,
		out.display()
	);
	std::process::exit(if problems > 0 { 1 } else { 0 });
}
