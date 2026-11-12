//! rac-rs PS4 port: the pipeline capture (PS4_GPU_DESIGN.md, "The shader cache key and the capture").
//!
//! With `RAC_PS4_CAPTURE=DIR`, every render and compute pipeline the game creates is written to DIR as a record that the
//! WSL compiler (`ps4/tools/vkc/vkrec.c`) replays on Mesa radv's GFX7 device to get the PS4's GCN code:
//!   - `<key>.rec`: text, Vulkan enum values as numbers (format below);
//!   - `<key>.<vs|fs|cs>.spv`: the SPIR-V this back end made for the stage (with its binding map applied).
//! `<key>` is `crate::ps4key`'s pipeline key in 16 hex digits, the name the PS4's GNM back end looks pipelines up by.
//! Nothing here runs unless the variable is set, apart from one environment lookup.
//!
//! Record lines (one item per line, in this order):
//!   rec 1 / key K / label L / kind render|compute / layout LK IMMEDIATE_SIZE
//!   set INDEX BGL_KEY NBINDINGS, then per binding: b VK_BINDING VK_TYPE COUNT VK_STAGES WGPU_BINDING
//!   stage VK_STAGE ENTRY SPV_FILE
//!   render only: vb BINDING STRIDE RATE / va LOCATION BINDING VK_FORMAT OFFSET / ia TOPOLOGY RESTART /
//!     rs POLYGON CULL FRONT DEPTH_CLAMP BIAS_ON BIAS_CONST BIAS_CLAMP BIAS_SLOPE (floats as bits) /
//!     ds VK_FORMAT TEST WRITE COMPARE STENCIL_ON, then sf FACE(0 front, 1 back) FAIL PASS DEPTH_FAIL COMPARE READ WRITE /
//!     ms SAMPLES ALPHA_TO_COVERAGE MASK_LO MASK_HI /
//!     cb INDEX VK_FORMAT BLEND SRC_C DST_C OP_C SRC_A DST_A OP_A WRITE_MASK (VK_FORMAT 0 = an unused slot)
//!   end

use alloc::{format, string::String, vec::Vec};
use core::fmt::Write as _;
use std::{
	collections::{HashMap, HashSet},
	path::PathBuf,
	sync::{Mutex, OnceLock},
};

use ash::vk::{self, Handle};

/// The capture folder, or `None` when capturing is off.
pub(super) fn dir() -> Option<&'static PathBuf> {
	static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
	DIR.get_or_init(|| {
		let d = std::env::var_os("RAC_PS4_CAPTURE").filter(|v| !v.is_empty()).map(PathBuf::from)?;
		match std::fs::create_dir_all(&d) {
			Ok(()) => {
				log::info!("PS4 pipeline capture into {}", d.display());
				Some(d)
			}
			Err(e) => {
				log::warn!("PS4 pipeline capture: cannot create {}: {e}", d.display());
				None
			}
		}
	})
	.as_ref()
}

/// A bind group layout as recorded: its key and its binding lines.
struct SetRec {
	key: u64,
	text: String,
}

/// A pipeline layout as recorded.
struct LayoutRec {
	key: u64,
	immediate_size: u32,
	text: String,
}

static SETS: Mutex<Option<HashMap<u64, SetRec>>> = Mutex::new(None);
static LAYOUTS: Mutex<Option<HashMap<u64, LayoutRec>>> = Mutex::new(None);
static WRITTEN: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

/// Called from `create_bind_group_layout` with what it built.
pub(super) fn bind_group_layout(
	raw: vk::DescriptorSetLayout,
	desc: &crate::BindGroupLayoutDescriptor<'_>,
	vk_bindings: &[vk::DescriptorSetLayoutBinding<'_>],
	binding_map: &[(u32, super::BindingInfo)],
) {
	let mut text = String::new();
	for (b, (wgpu_binding, _)) in vk_bindings.iter().zip(binding_map) {
		let _ = writeln!(
			text,
			"b {} {} {} {} {}",
			b.binding,
			b.descriptor_type.as_raw(),
			b.descriptor_count,
			b.stage_flags.as_raw(),
			wgpu_binding
		);
	}
	let rec = SetRec { key: crate::ps4key::bind_group_layout(desc), text };
	SETS.lock().unwrap().get_or_insert_with(HashMap::new).insert(raw.as_raw(), rec);
}

/// Called from `create_pipeline_layout`.
pub(super) fn pipeline_layout(raw: vk::PipelineLayout, desc: &crate::PipelineLayoutDescriptor<'_, super::BindGroupLayout>) {
	let sets = SETS.lock().unwrap();
	let sets = sets.as_ref();
	let mut keys = Vec::new();
	let mut text = String::new();
	for (i, g) in desc.bind_group_layouts.iter().enumerate() {
		match g.and_then(|g| sets.and_then(|s| s.get(&g.raw.as_raw()))) {
			Some(s) => {
				keys.push(Some(s.key));
				let _ = write!(text, "set {i} {:016x} {}\n{}", s.key, s.text.lines().count(), s.text);
			}
			None => {
				// An empty group (the back end binds an empty set layout there).
				keys.push(None);
				let _ = writeln!(text, "set {i} 0 0");
			}
		}
	}
	let rec = LayoutRec { key: crate::ps4key::pipeline_layout(&keys, desc.immediate_size), immediate_size: desc.immediate_size, text };
	LAYOUTS.lock().unwrap().get_or_insert_with(HashMap::new).insert(raw.as_raw(), rec);
}

/// What `compile_stage` keeps for the capture: the stage key and the SPIR-V.
pub(super) struct StageCap {
	pub key: u64,
	pub spv: Vec<u32>,
}

/// The stage key of `stage` (the naga module it was made from).
pub(super) fn stage_key(naga_shader: &crate::NagaShader, stage: &crate::ProgrammableStage<'_, super::ShaderModule>) -> u64 {
	crate::ps4key::stage(&naga_shader.module, stage.entry_point, stage.constants, stage.zero_initialize_workgroup_memory)
}

/// One stage of a pipeline for the record: Vulkan stage, file tag, entry point and what `compile_stage` captured.
pub(super) struct StageIn<'a> {
	pub vk_stage: vk::ShaderStageFlags,
	pub tag: &'static str,
	pub entry: &'a str,
	pub cap: Option<&'a StageCap>,
}

/// The fixed state of a render pipeline, as the Vulkan back end built it.
pub(super) struct RenderIn<'a> {
	pub vertex_buffers: &'a [vk::VertexInputBindingDescription],
	pub vertex_attributes: &'a [vk::VertexInputAttributeDescription],
	pub topology: vk::PrimitiveTopology,
	pub restart: bool,
	pub rs: &'a vk::PipelineRasterizationStateCreateInfo<'a>,
	pub ds: &'a vk::PipelineDepthStencilStateCreateInfo<'a>,
	pub ds_format: vk::Format,
	pub samples: u32,
	pub alpha_to_coverage: bool,
	pub sample_mask: u64,
	pub blend: &'a [vk::PipelineColorBlendAttachmentState],
	pub color_formats: &'a [vk::Format],
}

fn header(key: u64, label: crate::Label<'_>, kind: &str, layout: &LayoutRec) -> String {
	// Labels go on one line, so no line breaks in them.
	let label = label.unwrap_or("-").replace(['\n', '\r'], " ");
	format!(
		"rec 1\nkey {key:016x}\nlabel {label}\nkind {kind}\nlayout {:016x} {}\n{}",
		layout.key, layout.immediate_size, layout.text
	)
}

/// Writes the stages' SPIR-V and appends their lines; false when one has no capture (a raw SPIR-V module).
fn stages(dir: &std::path::Path, key: u64, stages: &[StageIn<'_>], text: &mut String) -> bool {
	for s in stages {
		let Some(cap) = s.cap else {
			return false;
		};
		let file = format!("{key:016x}.{}.spv", s.tag);
		let bytes: Vec<u8> = cap.spv.iter().flat_map(|w| w.to_le_bytes()).collect();
		if let Err(e) = std::fs::write(dir.join(&file), bytes) {
			log::warn!("PS4 pipeline capture: {file}: {e}");
			return false;
		}
		let _ = writeln!(text, "stage {} {} {file}", s.vk_stage.as_raw(), s.entry);
	}
	true
}

/// True the first time `key` is seen (pipelines are often created again with the same key).
fn first_time(key: u64) -> bool {
	WRITTEN.lock().unwrap().get_or_insert_with(HashSet::new).insert(key)
}

fn finish(dir: &std::path::Path, key: u64, mut text: String) {
	text.push_str("end\n");
	let file = format!("{key:016x}.rec");
	if let Err(e) = std::fs::write(dir.join(&file), text) {
		log::warn!("PS4 pipeline capture: {file}: {e}");
	}
}

/// Records a render pipeline. `key_state` is the back-end independent part of its key.
pub(super) fn render(
	label: crate::Label<'_>,
	layout: vk::PipelineLayout,
	ins: &[StageIn<'_>],
	key_state: &crate::ps4key::RenderState<'_>,
	r: &RenderIn<'_>,
) {
	let Some(dir) = dir() else {
		return;
	};
	let layouts = LAYOUTS.lock().unwrap();
	let Some(l) = layouts.as_ref().and_then(|m| m.get(&layout.as_raw())) else {
		log::warn!("PS4 pipeline capture: render pipeline {label:?} has an unknown layout");
		return;
	};
	let Some(stage_keys) = ins.iter().map(|s| s.cap.map(|c| (s.tag, c.key))).collect::<Option<Vec<_>>>() else {
		log::warn!("PS4 pipeline capture: render pipeline {label:?} uses a raw SPIR-V module, skipped");
		return;
	};
	let key = crate::ps4key::render(l.key, &stage_keys, key_state);
	if !first_time(key) {
		return;
	}
	let mut t = header(key, label, "render", l);
	drop(layouts);
	if !stages(dir, key, ins, &mut t) {
		return;
	}
	for b in r.vertex_buffers {
		let _ = writeln!(t, "vb {} {} {}", b.binding, b.stride, b.input_rate.as_raw());
	}
	for a in r.vertex_attributes {
		let _ = writeln!(t, "va {} {} {} {}", a.location, a.binding, a.format.as_raw(), a.offset);
	}
	let _ = writeln!(t, "ia {} {}", r.topology.as_raw(), r.restart as u32);
	let rs = r.rs;
	let _ = writeln!(
		t,
		"rs {} {} {} {} {} {} {} {}",
		rs.polygon_mode.as_raw(),
		rs.cull_mode.as_raw(),
		rs.front_face.as_raw(),
		rs.depth_clamp_enable,
		rs.depth_bias_enable,
		rs.depth_bias_constant_factor.to_bits(),
		rs.depth_bias_clamp.to_bits(),
		rs.depth_bias_slope_factor.to_bits()
	);
	let ds = r.ds;
	let _ = writeln!(
		t,
		"ds {} {} {} {} {}",
		r.ds_format.as_raw(),
		ds.depth_test_enable,
		ds.depth_write_enable,
		ds.depth_compare_op.as_raw(),
		ds.stencil_test_enable
	);
	for (i, f) in [ds.front, ds.back].iter().enumerate() {
		let _ = writeln!(
			t,
			"sf {i} {} {} {} {} {} {}",
			f.fail_op.as_raw(),
			f.pass_op.as_raw(),
			f.depth_fail_op.as_raw(),
			f.compare_op.as_raw(),
			f.compare_mask,
			f.write_mask
		);
	}
	let _ = writeln!(t, "ms {} {} {} {}", r.samples, r.alpha_to_coverage as u32, r.sample_mask as u32, (r.sample_mask >> 32) as u32);
	for (i, (b, f)) in r.blend.iter().zip(r.color_formats).enumerate() {
		let _ = writeln!(
			t,
			"cb {i} {} {} {} {} {} {} {} {} {}",
			f.as_raw(),
			b.blend_enable,
			b.src_color_blend_factor.as_raw(),
			b.dst_color_blend_factor.as_raw(),
			b.color_blend_op.as_raw(),
			b.src_alpha_blend_factor.as_raw(),
			b.dst_alpha_blend_factor.as_raw(),
			b.alpha_blend_op.as_raw(),
			b.color_write_mask.as_raw()
		);
	}
	finish(dir, key, t);
}

/// Records a compute pipeline.
pub(super) fn compute(label: crate::Label<'_>, layout: vk::PipelineLayout, s: &StageIn<'_>) {
	let Some(dir) = dir() else {
		return;
	};
	let layouts = LAYOUTS.lock().unwrap();
	let Some(l) = layouts.as_ref().and_then(|m| m.get(&layout.as_raw())) else {
		log::warn!("PS4 pipeline capture: compute pipeline {label:?} has an unknown layout");
		return;
	};
	let Some(cap) = s.cap else {
		log::warn!("PS4 pipeline capture: compute pipeline {label:?} uses a raw SPIR-V module, skipped");
		return;
	};
	let key = crate::ps4key::compute(l.key, cap.key);
	if !first_time(key) {
		return;
	}
	let mut t = header(key, label, "compute", l);
	drop(layouts);
	if stages(dir, key, core::slice::from_ref(s), &mut t) {
		finish(dir, key, t);
	}
}
