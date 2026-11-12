//! Pipelines from the shipped shader cache (`rac_ps4_gnm::cache`, made on the PC by `ps4/tools/shcache.sh`).
//!
//! The console cannot compile shaders. A pipeline is found by `crate::ps4key`'s key, which the PC capture computed from the
//! same descriptors; its compiled stages (.rsh) go to GPU memory once, and the fixed state (blend, depth, raster) comes from
//! the hal descriptor. A key that is not in the cache gives a "missing" pipeline: it is logged and listed in the missing
//! list (feed it back to the PC capture), and draws with it are skipped.
//!
//! Bind group layouts get radv's descriptor set memory layout from the same file: every cached pipeline names its sets'
//! layout keys, and its `.sets` text has their binding offsets.

use alloc::{
	boxed::Box,
	format,
	string::{String, ToString},
	sync::Arc,
	vec::Vec,
};
use std::collections::{HashMap, HashSet};

use parking_lot::Mutex;
use rac_ps4_gnm::cache::Cache;
use rac_ps4_gnm::shader::{Shader, Stage};

/// Where one binding of a set lives in radv's set memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
	/// Bytes from the start of the set.
	Offset(u32),
	/// A dynamic buffer: n-th dynamic binding of this set (radv keeps them outside set memory).
	Dynamic(u32),
}

/// radv's memory layout of one descriptor set (bind group layout).
#[derive(Clone, Debug, Default)]
pub struct SetLayout {
	pub size: u32,
	pub slots: HashMap<u32, Slot>,
	pub dynamic_count: u32,
}

/// The shader cache file, loaded once.
pub struct ShaderCache {
	cache: Option<Cache<'static>>,
	sets: HashMap<u64, Arc<SetLayout>>,
	missing: Mutex<HashSet<u64>>,
	missing_file: Option<std::path::PathBuf>,
}

fn cache_path() -> std::path::PathBuf {
	if let Some(p) = std::env::var_os("RAC_PS4_SHADERS") {
		return p.into();
	}
	if cfg!(target_os = "freebsd") {
		"/app0/ps4shaders.bin".into()
	} else {
		"build-ps4/ps4shaders.bin".into()
	}
}

impl ShaderCache {
	pub fn load() -> ShaderCache {
		let path = cache_path();
		let missing_file = std::env::var_os("RAC_PS4_MISSING")
			.map(Into::into)
			.or_else(|| cfg!(target_os = "freebsd").then(|| "/data/rac-rs/missing_pipelines.txt".into()));
		let bytes = match std::fs::read(&path) {
			Ok(b) => b,
			Err(e) => {
				log::error!("GNM: no shader cache at {} ({e}): every pipeline will be missing", path.display());
				return ShaderCache { cache: None, sets: HashMap::new(), missing: Mutex::new(HashSet::new()), missing_file };
			}
		};
		// The cache lives for the program: entries and parsed shaders borrow it.
		let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
		let cache = match Cache::parse(bytes) {
			Ok(c) => c,
			Err(e) => {
				log::error!("GNM: shader cache {} is damaged: {e:?}", path.display());
				return ShaderCache { cache: None, sets: HashMap::new(), missing: Mutex::new(HashSet::new()), missing_file };
			}
		};
		let mut sets = HashMap::new();
		for key in cache.keys().collect::<Vec<_>>() {
			let Ok(Some(e)) = cache.get(key) else { continue };
			for (k, l) in set_layouts(e.rec, e.sets) {
				sets.entry(k).or_insert_with(|| Arc::new(l));
			}
		}
		log::info!("GNM: shader cache {}: {} pipelines, {} set layouts", path.display(), cache.len(), sets.len());
		ShaderCache { cache: Some(cache), sets, missing: Mutex::new(HashSet::new()), missing_file }
	}

	/// radv's layout of the bind group layout with `key`, if any cached pipeline uses it.
	pub fn set_layout(&self, key: u64) -> Option<Arc<SetLayout>> {
		self.sets.get(&key).cloned()
	}

	/// The compiled stages of pipeline `key` (record order), or `None` after noting it as missing.
	pub fn stages(&self, key: u64, label: crate::Label<'_>) -> Option<Vec<Shader<'static>>> {
		if skip_listed(key, label) {
			if self.missing.lock().insert(key) {
				std::eprintln!("GNM: pipeline {key:016x} ({}) skipped by RAC_GNM_SKIP; its draws are skipped", label.unwrap_or("-"));
			}
			return None;
		}
		let found = self.cache.as_ref().and_then(|c| c.get(key).ok().flatten());
		let Some(e) = found else {
			self.note_missing(key, label);
			return None;
		};
		match e.stages.iter().map(|b| Shader::parse(b)).collect::<Result<Vec<_>, _>>() {
			Ok(s) => Some(s),
			Err(err) => {
				log::error!("GNM: pipeline {key:016x} ({label:?}): bad stage in the cache: {err:?}");
				None
			}
		}
	}

	fn note_missing(&self, key: u64, label: crate::Label<'_>) {
		if !self.missing.lock().insert(key) {
			return;
		}
		log::warn!("GNM: pipeline {key:016x} ({}) is not in the shader cache; its draws are skipped", label.unwrap_or("-"));
		if let Some(f) = &self.missing_file {
			use std::io::Write;
			if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
				let _ = writeln!(f, "{key:016x} {}", label.unwrap_or("-"));
			}
		}
	}
}

/// Console diagnostics: `RAC_GNM_SKIP=label part,label part,0123456789abcdef` (env.txt) treats the pipelines whose label contains
/// one of the parts (any case; `=name` matches the whole label only), or whose key is listed, as missing, so their draws are skipped. That finds which pass hangs the
/// GPU or costs the frame time without a new package.
fn skip_listed(key: u64, label: crate::Label<'_>) -> bool {
	static LIST: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
	let list = LIST.get_or_init(|| {
		std::env::var("RAC_GNM_SKIP").map(|v| v.split(',').map(|p| p.trim().to_ascii_lowercase()).filter(|p| !p.is_empty()).collect()).unwrap_or_default()
	});
	if list.is_empty() {
		return false;
	}
	let label = label.unwrap_or("");
	// wgpu's own pipelines are never skipped. Its indirect-draw validation writes the draw arguments the indirect draws then read
	// (the sun shadow map's draws); skipped, those draws read whatever the buffer's memory held, and round 5's run 5 hung the GPU
	// that way (logged as the shadow map pipeline `vs_sh`, the only new one: the skipped pipeline is not in that list).
	if label.starts_with("(wgpu internal)") {
		if label_matches(list, key, label) {
			static ONCE: std::sync::Once = std::sync::Once::new();
			ONCE.call_once(|| std::eprintln!("GNM: RAC_GNM_SKIP leaves wgpu's own pipelines in ({label}): the draws that use them would hang the GPU"));
		}
		return false;
	}
	label_matches(list, key, label)
}

/// Whether a pipeline matches one of `parts` (lower case): a part of its label, `=label` exactly, or its key in hex.
pub fn label_matches(parts: &[String], key: u64, label: &str) -> bool {
	let hex = alloc::format!("{key:016x}");
	let label = label.to_ascii_lowercase();
	parts.iter().any(|p| match p.strip_prefix('=') {
		Some(exact) => label == exact,
		None => *p == hex || (!label.is_empty() && label.contains(p.as_str())),
	})
}

/// Console diagnostics, `RAC_GNM_AB_SKIP=45-59:vs_props;60-74:vs_tfrag|=vs`: while the shown-frame count is inside a window, the
/// pipelines matching that window's parts (`|`-separated, as in `RAC_GNM_SKIP`) draw nothing. With pictures of one frame per
/// window (`RAC_PS4_SHOT`), one run shows what each group of pipelines contributes, e.g. which ones come out black.
pub fn ab_skipped(frame: u64, key: u64, label: &str) -> bool {
	static W: std::sync::OnceLock<Vec<(u64, u64, Vec<String>)>> = std::sync::OnceLock::new();
	let w = W.get_or_init(|| {
		let v = std::env::var("RAC_GNM_AB_SKIP").unwrap_or_default();
		v.split(';')
			.filter_map(|win| {
				let (range, parts) = win.split_once(':')?;
				let (a, b) = range.trim().split_once('-')?;
				let parts = parts.split('|').map(|p| p.trim().to_ascii_lowercase()).filter(|p| !p.is_empty()).collect();
				Some((a.trim().parse().ok()?, b.trim().parse().ok()?, parts))
			})
			.collect()
	});
	w.iter().any(|(a, b, parts)| (*a..=*b).contains(&frame) && label_matches(parts, key, label))
}

/// The (layout key, layout) pairs of one cache entry: the record's `set I KEY N` lines, the `.sets` offsets.
fn set_layouts(rec: &str, sets: &str) -> Vec<(u64, SetLayout)> {
	let mut keys: HashMap<u32, u64> = HashMap::new();
	for l in rec.lines() {
		let w: Vec<&str> = l.split_whitespace().collect();
		if let ["set", i, k, _n] = w.as_slice() {
			if let (Ok(i), Ok(k)) = (i.parse::<u32>(), u64::from_str_radix(k, 16)) {
				// Key 0 = an empty group (no bind group layout).
				if k != 0 {
					keys.insert(i, k);
				}
			}
		}
	}
	let mut by_set: HashMap<u32, SetLayout> = HashMap::new();
	for l in sets.lines() {
		let w: Vec<&str> = l.split_whitespace().collect();
		match w.as_slice() {
			["set", i, size] => {
				if let (Ok(i), Ok(size)) = (i.parse(), size.parse()) {
					by_set.entry(i).or_default().size = size;
				}
			}
			["b", i, b, off] => {
				if let (Ok(i), Ok(b), Ok(off)) = (i.parse(), b.parse(), off.parse()) {
					by_set.entry(i).or_default().slots.insert(b, Slot::Offset(off));
				}
			}
			["dyn", i, b, _pipeline_index] => {
				if let (Ok(i), Ok(b)) = (i.parse(), b.parse()) {
					let s = by_set.entry(i).or_default();
					let n = s.dynamic_count;
					s.slots.insert(b, Slot::Dynamic(n));
					s.dynamic_count += 1;
				}
			}
			_ => {}
		}
	}
	keys.into_iter().filter_map(|(i, k)| by_set.remove(&i).map(|l| (k, l))).collect()
}

/// The vk binding numbers in the record are the bind group layout's entries in order; the cache keeps wgpu's binding number
/// as the record's last column, which is what a bind group names. This maps the `.sets` offsets (by vk binding) to wgpu
/// bindings, using the bind group layout's own entry order (sorted by binding, as the Vulkan back end numbers them).
pub fn remap_to_wgpu(layout: &SetLayout, entries: &[wgt::BindGroupLayoutEntry]) -> SetLayout {
	let mut out = SetLayout { size: layout.size, slots: HashMap::new(), dynamic_count: layout.dynamic_count };
	for (vk_binding, e) in entries.iter().enumerate() {
		if let Some(s) = layout.slots.get(&(vk_binding as u32)) {
			out.slots.insert(e.binding, *s);
		}
	}
	out
}

/// The do-nothing pixel shader for pipelines without a fragment stage (depth-only passes): `s_endpgm`, no inputs used.
pub const NOOP_PS_CODE: [u32; 1] = [0xbf81_0000];

/// Frame trace, `RAC_GNM_TRACE_PIPES=1`: every pipeline bound in a traced frame goes to the log, not only the overlay ones.
pub fn trace_all_pipes() -> bool {
	static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
	*ON.get_or_init(|| std::env::var("RAC_GNM_TRACE_PIPES").is_ok_and(|v| v == "1"))
}

/// Frame trace: the pipelines whose binds and texture descriptors the trace prints by default: the HUD / retail overlay
/// (`vs_hud*`, "overlay depth") and the modern UI (labels "ui ...": its menus and the title's text, ui/gpu_pipes.rs).
pub fn overlay_like(label: &str, entries: &str) -> bool {
	entries.contains("hud") || label.contains("overlay") || label == "ui" || label.starts_with("ui ")
}

/// `RAC_GNM_DUMP_UPLOADS=N[,FROM]`: true for the first N large texture uploads (see `copy_buffer_to_texture`) recorded in
/// frame FROM (default 1) or later, so the start-up textures do not use them up.
pub fn dump_upload(frame: u64) -> bool {
	static CFG: std::sync::OnceLock<(core::sync::atomic::AtomicU32, u64)> = std::sync::OnceLock::new();
	let (left, from) = CFG.get_or_init(|| {
		let v = std::env::var("RAC_GNM_DUMP_UPLOADS").unwrap_or_default();
		let mut it = v.split(',').map(|x| x.trim().parse::<u64>().ok());
		let n = it.next().flatten().unwrap_or(0) as u32;
		(core::sync::atomic::AtomicU32::new(n), it.next().flatten().unwrap_or(1))
	});
	frame >= *from && left.try_update(core::sync::atomic::Ordering::Relaxed, core::sync::atomic::Ordering::Relaxed, |n| n.checked_sub(1)).is_ok()
}

/// A frame list from an env var: comma-separated frame numbers and ranges `A..B/STEP` (every STEP-th frame from A up to B;
/// `/STEP` may be left out = every frame), e.g. `RAC_GNM_GPUPROF=130,400..3000/100`.
pub struct Frames(Vec<(u64, u64, u64)>);

impl Frames {
	pub fn parse(s: &str) -> Frames {
		let mut v = Vec::new();
		for t in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
			let (range, step) = t.split_once('/').map_or((t, 1), |(r, st)| (r, st.trim().parse().unwrap_or(1).max(1)));
			match range.split_once("..") {
				Some((a, b)) => {
					if let (Ok(a), Ok(b)) = (a.trim().parse(), b.trim().parse()) {
						v.push((a, b, step));
					}
				}
				None => {
					if let Ok(n) = range.parse() {
						v.push((n, n, 1));
					}
				}
			}
		}
		Frames(v)
	}

	pub fn env(name: &str) -> Frames {
		Frames::parse(&std::env::var(name).unwrap_or_default())
	}

	pub fn has(&self, n: u64) -> bool {
		self.0.iter().any(|&(a, b, s)| n >= a && n <= b && (n - a) % s == 0)
	}
}

/// A label for logs.
pub fn label_str(l: crate::Label<'_>) -> String {
	l.unwrap_or("-").to_string()
}

/// Parses `Stage` names for logs.
pub fn stage_name(s: Stage) -> &'static str {
	match s {
		Stage::Vertex => "vertex",
		Stage::Fragment => "fragment",
		Stage::Compute => "compute",
	}
}

/// A short description of why a pipeline cannot be used (logged once).
pub fn mismatch(key: u64, what: &str) -> String {
	format!("GNM: pipeline {key:016x}: {what}")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn set_layouts_from_record() {
		let rec = "rec 1\nset 0 00000000000000aa 2\nb 0 6 1 17 0\nb 1 8 1 16 3\nset 1 0 0\nstage 1 vs a.spv\n";
		let sets = "set 0 96\nb 0 0 64\ndyn 0 1 0\nset 1 0\n";
		let l = set_layouts(rec, sets);
		assert_eq!(l.len(), 1);
		let (k, s) = &l[0];
		assert_eq!(*k, 0xaa);
		assert_eq!(s.size, 96);
		assert_eq!(s.slots[&0], Slot::Offset(64));
		assert_eq!(s.slots[&1], Slot::Dynamic(0));
		assert_eq!(s.dynamic_count, 1);
	}
}
