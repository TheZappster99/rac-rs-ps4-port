//! wgsl2spv FILE.wgsl OUTDIR [ENTRY...]: each entry point (all when none are named) of FILE as OUTDIR/<file>.<entry>.spv,
//! with OUTDIR/<file>.<entry>.txt describing what a Vulkan pipeline for it needs:
//!   stage vertex|fragment|compute
//!   bind SET BINDING KIND COUNT      (KIND: uniform storage storage_ro texture storage_texture sampler)
//!   in LOCATION FORMAT                (vertex inputs; FORMAT: f32 f32x2 f32x3 f32x4 u32 u32x2 ... i32...)
//!   out LOCATION                      (fragment colour outputs)
//!   depth                             (the fragment shader writes depth)
//! Each entry point is compiled from a copy of the module with the other entry points removed and the unreachable items
//! compacted away, so code an entry does not use (ray queries, other passes) does not reach its SPIR-V.

use naga::{AddressSpace, Binding, BuiltIn, ImageClass, Module, ScalarKind, ShaderStage, StorageAccess, TypeInner};
use std::fmt::Write;

fn main() {
	let a: Vec<String> = std::env::args().skip(1).collect();
	if a.len() < 2 {
		eprintln!("usage: wgsl2spv FILE.wgsl OUTDIR [ENTRY...]");
		std::process::exit(2);
	}
	let src = std::fs::read_to_string(&a[0]).expect("read wgsl");
	let stem = std::path::Path::new(&a[0]).file_stem().unwrap().to_string_lossy().into_owned();
	let out = &a[1];
	std::fs::create_dir_all(out).unwrap();
	let module = match naga::front::wgsl::parse_str(&src) {
		Ok(m) => m,
		Err(e) => {
			eprintln!("{}: parse error:\n{}", a[0], e.emit_to_string(&src));
			std::process::exit(1);
		}
	};
	let wanted: Vec<&String> = a[2..].iter().collect();
	let mut fails = 0;
	for ep in &module.entry_points {
		if !wanted.is_empty() && !wanted.iter().any(|w| **w == ep.name) {
			continue;
		}
		match one(&module, &ep.name, ep.stage) {
			Ok((spv, info)) => {
				let base = format!("{out}/{stem}.{}", ep.name);
				let bytes: Vec<u8> = spv.iter().flat_map(|w| w.to_le_bytes()).collect();
				std::fs::write(format!("{base}.spv"), bytes).unwrap();
				std::fs::write(format!("{base}.txt"), info).unwrap();
				println!("{stem}.{}: {} words", ep.name, spv.len());
			}
			Err(e) => {
				fails += 1;
				println!("{stem}.{}: FAILED: {e}", ep.name);
			}
		}
	}
	if fails > 0 {
		std::process::exit(1);
	}
}

fn one(module: &Module, entry: &str, stage: ShaderStage) -> Result<(Vec<u32>, String), String> {
	let mut m = module.clone();
	m.entry_points.retain(|e| e.name == entry);
	naga::compact::compact(&mut m, naga::compact::KeepUnused::No);
	let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
		.validate(&m)
		.map_err(|e| format!("validation: {e:?}"))?;
	let opts = naga::back::spv::Options { lang_version: (1, 3), flags: naga::back::spv::WriterFlags::empty(), ..Default::default() };
	let popts = naga::back::spv::PipelineOptions { shader_stage: stage, entry_point: entry.to_string() };
	let spv = naga::back::spv::write_vec(&m, &info, &opts, Some(&popts)).map_err(|e| format!("spv: {e:?}"))?;
	Ok((spv, reflect(&m, stage)))
}

fn reflect(m: &Module, stage: ShaderStage) -> String {
	let mut s = String::new();
	let _ = writeln!(s, "stage {}", match stage {
		ShaderStage::Vertex => "vertex",
		ShaderStage::Fragment => "fragment",
		_ => "compute",
	});
	for (_, g) in m.global_variables.iter() {
		let Some(rb) = &g.binding else { continue };
		let (inner, count) = match &m.types[g.ty].inner {
			TypeInner::BindingArray { base, size } => (&m.types[*base].inner, match size {
				naga::ArraySize::Constant(n) => n.get(),
				_ => 1,
			}),
			t => (t, 1),
		};
		let kind = match (g.space, inner) {
			(AddressSpace::Uniform, _) => "uniform",
			(AddressSpace::Storage { access }, _) => {
				if access.contains(StorageAccess::STORE) { "storage" } else { "storage_ro" }
			}
			(_, TypeInner::Image { class: ImageClass::Storage { .. }, .. }) => "storage_texture",
			(_, TypeInner::Image { .. }) => "texture",
			(_, TypeInner::Sampler { .. }) => "sampler",
			(_, TypeInner::AccelerationStructure { .. }) => "accel",
			_ => "other",
		};
		let _ = writeln!(s, "bind {} {} {kind} {count}", rb.group, rb.binding);
	}
	let ep = &m.entry_points[0];
	let fmt = |t: &TypeInner| -> String {
		let (k, w, n) = match t {
			TypeInner::Scalar(sc) => (sc.kind, sc.width, 1),
			TypeInner::Vector { size, scalar } => (scalar.kind, scalar.width, *size as u8),
			_ => return "?".into(),
		};
		let k = match k {
			ScalarKind::Float => "f",
			ScalarKind::Uint => "u",
			ScalarKind::Sint => "i",
			_ => "?",
		};
		if n == 1 { format!("{k}{}", w * 8) } else { format!("{k}{}x{n}", w * 8) }
	};
	if stage == ShaderStage::Vertex {
		for arg in &ep.function.arguments {
			if let Some(Binding::Location { location, .. }) = arg.binding {
				let _ = writeln!(s, "in {location} {}", fmt(&m.types[arg.ty].inner));
			}
			if let TypeInner::Struct { members, .. } = &m.types[arg.ty].inner {
				for mem in members {
					if let Some(Binding::Location { location, .. }) = mem.binding {
						let _ = writeln!(s, "in {location} {}", fmt(&m.types[mem.ty].inner));
					}
				}
			}
		}
	}
	if stage == ShaderStage::Fragment {
		if let Some(r) = &ep.function.result {
			let mut outs = |b: &Option<Binding>| match b {
				Some(Binding::Location { location, .. }) => {
					let _ = writeln!(s, "out {location}");
				}
				Some(Binding::BuiltIn(BuiltIn::FragDepth)) => {
					let _ = writeln!(s, "depth");
				}
				_ => {}
			};
			outs(&r.binding);
			if let TypeInner::Struct { members, .. } = &m.types[r.ty].inner {
				for mem in members {
					outs(&mem.binding);
				}
			}
		}
	}
	s
}
