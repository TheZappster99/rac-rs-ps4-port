//! rac-rs PS4 port: the shader-cache key of a pipeline (PS4_GPU_DESIGN.md, "The shader cache key and the capture").
//!
//! The PS4 has no shader compiler, so every pipeline the renderer creates is compiled on the PC ahead of time and shipped in
//! a cache. The PC capture (Vulkan back end, `RAC_PS4_CAPTURE`) and the PS4's GNM back end must name a pipeline the same
//! way, from what both see: the hal descriptors and the naga modules. So this file only uses back-end independent data.
//!
//! The hash is FNV-1a 64 over `Debug` text, streamed (no allocation). `Debug` output is deterministic for the same input
//! and the same wgpu / naga sources, which both builds share (vendored). naga's `Block` prints its spans when naga's `span`
//! feature is on, so both builds must enable the same naga features; the spans are the same for the same WGSL source.

use core::fmt::{self, Write};

/// A streaming FNV-1a 64 hasher that `Debug` output can be written into.
#[derive(Debug)]
pub struct KeyHasher(u64);

impl KeyHasher {
	pub fn new(tag: &str) -> Self {
		let mut h = KeyHasher(0xcbf2_9ce4_8422_2325);
		h.bytes(tag.as_bytes());
		h.bytes(&[0xff]);
		h
	}

	fn bytes(&mut self, b: &[u8]) {
		for &x in b {
			self.0 ^= x as u64;
			self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
		}
	}

	/// Mixes in the `Debug` text of `v`, followed by a separator.
	pub fn debug(&mut self, v: &dyn fmt::Debug) -> &mut Self {
		let _ = write!(self, "{v:?}");
		self.bytes(&[0xfe]);
		self
	}

	pub fn u64(&mut self, v: u64) -> &mut Self {
		self.bytes(&v.to_le_bytes());
		self
	}

	pub fn str(&mut self, s: &str) -> &mut Self {
		self.bytes(s.as_bytes());
		self.bytes(&[0xfd]);
		self
	}

	pub fn finish(&self) -> u64 {
		self.0
	}
}

impl Write for KeyHasher {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		self.bytes(s.as_bytes());
		Ok(())
	}
}

/// One programmable stage: the module, the entry point, the override constants (sorted by name) and the workgroup-memory
/// zeroing flag, which all change the compiled code.
pub fn stage(module: &naga::Module, entry_point: &str, constants: &naga::back::PipelineConstants, zero_init_workgroup: bool) -> u64 {
	let mut h = KeyHasher::new("stage");
	h.debug(module).str(entry_point);
	let mut c: alloc::vec::Vec<(&alloc::string::String, &f64)> = constants.iter().collect();
	c.sort_by(|a, b| a.0.cmp(b.0));
	for (k, v) in c {
		h.str(k).u64(v.to_bits());
	}
	h.u64(zero_init_workgroup as u64);
	let key = h.finish();
	// DEV (RAC_PS4_KEYDUMP, see `dump`): the hashed module text, to find what makes two builds of a shader differ.
	if let Some(dir) = std::env::var_os("RAC_PS4_KEYDUMP") {
		let p = std::path::Path::new(&dir).join(alloc::format!("stage_{key:016x}_{entry_point}.txt"));
		if !p.exists() {
			let _ = std::fs::write(p, alloc::format!("{module:#?}"));
		}
	}
	key
}

/// A bind group layout: its flags and entries (the label does not count).
pub fn bind_group_layout(desc: &crate::BindGroupLayoutDescriptor<'_>) -> u64 {
	let mut h = KeyHasher::new("bgl");
	h.debug(&desc.flags).debug(&desc.entries);
	h.finish()
}

/// A pipeline layout from its bind group layouts' keys (`None` = an empty group) and the immediates size.
pub fn pipeline_layout(groups: &[Option<u64>], immediate_size: u32) -> u64 {
	let mut h = KeyHasher::new("pl");
	for g in groups {
		match g {
			Some(k) => h.u64(1).u64(*k),
			None => h.u64(0),
		};
	}
	h.u64(immediate_size as u64);
	h.finish()
}

/// What a render pipeline's key covers apart from its stages and layout.
#[derive(Debug)]
pub struct RenderState<'a> {
	pub vertex_buffers: &'a dyn fmt::Debug,
	pub primitive: &'a wgt::PrimitiveState,
	pub depth_stencil: &'a Option<wgt::DepthStencilState>,
	pub multisample: &'a wgt::MultisampleState,
	pub color_targets: &'a [Option<wgt::ColorTargetState>],
	pub multiview_mask: Option<core::num::NonZeroU32>,
}

/// A render pipeline: layout key, (stage tag, stage key) in pipeline order, and the fixed state.
pub fn render(layout: u64, stages: &[(&str, u64)], s: &RenderState<'_>) -> u64 {
	let mut h = KeyHasher::new("render");
	h.u64(layout);
	for (tag, k) in stages {
		h.str(tag).u64(*k);
	}
	h.debug(s.vertex_buffers)
		.debug(s.primitive)
		.debug(s.depth_stencil)
		.debug(s.multisample)
		.debug(&s.color_targets)
		.debug(&s.multiview_mask);
	let key = h.finish();
	dump(key, layout, stages, s);
	key
}

/// DEV: RAC_PS4_KEYDUMP=DIR writes `<key>.txt` per render pipeline (the layout and stage keys and the fixed state's text), so a
/// key the cache lacks can be compared with the captured one of the same shaders.
fn dump(key: u64, layout: u64, stages: &[(&str, u64)], s: &RenderState<'_>) {
	let Some(dir) = std::env::var_os("RAC_PS4_KEYDUMP") else { return };
	let mut t = alloc::format!("layout {layout:016x}\n");
	for (tag, k) in stages {
		t += &alloc::format!("stage {tag} {k:016x}\n");
	}
	t += &alloc::format!("{s:#?}\n");
	let _ = std::fs::write(std::path::Path::new(&dir).join(alloc::format!("{key:016x}.txt")), t);
}

/// A compute pipeline: layout key and stage key.
pub fn compute(layout: u64, stage: u64) -> u64 {
	let mut h = KeyHasher::new("compute");
	h.u64(layout).u64(stage);
	h.finish()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn stable_and_sensitive() {
		let a = pipeline_layout(&[Some(1), None], 16);
		assert_eq!(a, pipeline_layout(&[Some(1), None], 16));
		assert_ne!(a, pipeline_layout(&[Some(1), None], 32));
		assert_ne!(a, pipeline_layout(&[None, Some(1)], 16));
		assert_ne!(compute(1, 2), compute(2, 1));
	}
}
