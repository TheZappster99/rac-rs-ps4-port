//! Lists what the stages of a shader cache file take as user data: `cargo run --example cacheinfo -- build-ps4/ps4shaders.bin`.
//! A line per stage: key, stage index, user SGPR count, then each user-data slot as slot@sgpr+count.

use rac_ps4_gnm::{cache::Cache, shader::Shader};

fn main() {
	let path = std::env::args().nth(1).expect("usage: cacheinfo FILE");
	let data = std::fs::read(&path).expect("read the cache file");
	let cache = Cache::parse(&data).expect("parse the cache file");
	// `cacheinfo FILE KEY DIR`: writes each stage's machine code of that pipeline to DIR/KEY.N.code (for a disassembler).
	if let (Some(k), Some(dir)) = (std::env::args().nth(2), std::env::args().nth(3)) {
		let key = u64::from_str_radix(&k, 16).expect("key in hex");
		let e = cache.get(key).expect("entry").expect("key not in the cache");
		for (i, st) in e.stages.iter().enumerate() {
			let s = Shader::parse(st).expect("stage");
			std::fs::write(format!("{dir}/{k}.{i}.code"), s.code).expect("write the code");
		}
		return;
	}
	for key in cache.keys() {
		let e = cache.get(key).expect("entry").expect("listed key");
		for (i, st) in e.stages.iter().enumerate() {
			let s = Shader::parse(st).expect("stage");
			let mut line = format!("{key:016x} stage {i} sgprs {} scratch {}", s.user_sgprs(), s.scratch_bytes_per_wave);
			for slot in 0..rac_ps4_gnm::shader::ud::MAX {
				if let Some(l) = s.ud(slot) {
					line += &format!(" ud{slot}@{}+{}", l.sgpr, l.count);
				}
			}
			for set in 0..8 {
				if let Some(l) = s.set_sgpr(set) {
					line += &format!(" set{set}@{}", l.sgpr);
				}
			}
			// Vertex shaders: which locations they fetch (a gap means radv packs the descriptor table: see `vertex_table`).
			if i == 0 && s.vs_vb_desc_usage_mask != 0 {
				line += &format!(" vb_mask {:#b}", s.vs_vb_desc_usage_mask);
			}
			// Pixel shaders: what they export (a PS with no export at all can stall the GPU on GFX7).
			if i == 1 {
				line += &format!(" col_format {:#x} cb_mask {:#x} input_ena {:#x} rsrc2 {:#x}", s.ps_col_format, s.ps_cb_shader_mask, s.ps_input_ena, s.rsrc2);
			}
			println!("{line}");
		}
	}
}
