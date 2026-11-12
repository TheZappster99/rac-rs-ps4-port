//! Pipelines and command recording for the GNM back end. An encoder writes PM4 packets into a CPU vector; the queue copies
//! them into GPU memory at submit, after the driver's default state.
//!
//! Resource binding follows radv's GFX7 ABI as the cached shaders expect it (see `rac_ps4_gnm::shader::ud`): each stage's
//! user SGPRs get the scratch-ring table, each descriptor set's 32-bit address, the push-constant copy and inline values, the
//! dynamic-buffer descriptors, and for the vertex stage the vertex-buffer table and base vertex / first instance. They are
//! written at every draw for the bound pipeline (simple, and cheap next to the draw itself).

use alloc::{format, string::String, sync::Arc, vec, vec::Vec};

use rac_ps4_gnm::{
	desc, pm4,
	regs::{self, op},
	shader::{ud, Shader, Stage},
};

use super::{blit, format::Fmt, mem::Alloc, plat, Api, Buffer, ComputePipeline, Resource, Shared, Texture, TextureView};
use super::{BindGroup, PipelineLayout, RenderPipeline};

type DeviceResult<T> = Result<T, crate::DeviceError>;

// More registers (byte addresses; Mesa sid.h, CIK).
const CB_COLOR_STRIDE: u32 = 0x3c;
const CB_BLEND_RED: u32 = 0x28414;
const DB_STENCIL_CONTROL: u32 = 0x2842c;
const DB_STENCILREFMASK: u32 = 0x28430;
const PA_SC_VPORT_ZMIN_0: u32 = regs::PA_SC_VPORT_ZMIN_0;
const VGT_MULTI_PRIM_IB_RESET_INDX: u32 = 0x2840c;
const VGT_MULTI_PRIM_IB_RESET_EN: u32 = 0x28a94;
/// VGT_INSTANCE_STEP_RATE_0 / _1 (two consecutive context registers).
const VGT_INSTANCE_STEP_RATE_0: u32 = 0x28aa0;
const PA_SU_POLY_OFFSET_DB_FMT_CNTL: u32 = 0x28b78;
const PA_SU_POLY_OFFSET_CLAMP: u32 = 0x28b7c;
const COMPUTE_START_X: u32 = 0xb810;
const COMPUTE_NUM_THREAD_X: u32 = 0xb81c;
const COMPUTE_PGM_LO: u32 = 0xb830;
const COMPUTE_PGM_RSRC1: u32 = 0xb848;
const COMPUTE_RESOURCE_LIMITS: u32 = 0xb854;
const COMPUTE_STATIC_THREAD_MGMT_SE0: u32 = 0xb858;
const COMPUTE_TMPRING_SIZE: u32 = 0xb860;
const COMPUTE_STATIC_THREAD_MGMT_SE2: u32 = 0xb864;
/// Graphics scratch: WAVES (11:0) and WAVESIZE (24:12, in 1 KB units), as COMPUTE_TMPRING_SIZE for compute.
const SPI_TMPRING_SIZE: u32 = 0x286e8;
const COMPUTE_USER_DATA_0: u32 = 0xb900;

// More opcodes.
const OP_DRAW_INDEX_2: u32 = 0x27;
const OP_DISPATCH_DIRECT: u32 = 0x15;
const OP_DISPATCH_INDIRECT: u32 = 0x16;
const OP_SET_BASE: u32 = 0x11;
const OP_INDEX_BUFFER_SIZE: u32 = 0x13;
const OP_DRAW_INDIRECT: u32 = 0x24;
const OP_DRAW_INDEX_INDIRECT: u32 = 0x25;
const OP_INDEX_BASE: u32 = 0x26;
const OP_WAIT_REG_MEM: u32 = 0x3c;
const OP_EVENT_WRITE: u32 = 0x46;
const OP_DMA_DATA: u32 = 0x50;

/// The repeated-texel block of `fill_texels` (a multiple of 8 and 16 bytes, at most `UPLOAD_CHUNK`).
const FILL_BLOCK: usize = 64 << 10;

/// Largest CP DMA transfer (GFX7: 21-bit byte count, kept 8-aligned).
const DMA_MAX: u64 = (1 << 21) - 8;

/// A PM4 writer over a growable vector.
pub(crate) struct W<'a>(pub &'a mut Vec<u32>);

impl W<'_> {
	pub fn pkt3(&mut self, opcode: u32, body: &[u32]) {
		self.0.push(pm4::header(opcode, body.len()));
		self.0.extend_from_slice(body);
	}

	fn regs(&mut self, opcode: u32, base: u32, addr: u32, vals: &[u32]) {
		debug_assert!(addr >= base && !vals.is_empty());
		self.0.push(pm4::header(opcode, vals.len() + 1));
		self.0.push((addr - base) / 4);
		self.0.extend_from_slice(vals);
	}

	pub fn ctx(&mut self, addr: u32, vals: &[u32]) {
		self.regs(op::SET_CONTEXT_REG, 0x28000, addr, vals);
	}

	pub fn sh(&mut self, addr: u32, vals: &[u32]) {
		self.regs(op::SET_SH_REG, 0xb000, addr, vals);
	}

	pub fn uconfig(&mut self, addr: u32, vals: &[u32]) {
		self.regs(op::SET_UCONFIG_REG, 0x30000, addr, vals);
	}

	/// `n` dwords for a driver helper.
	pub fn reserve(&mut self, n: usize) -> &mut [u32] {
		let s = self.0.len();
		self.0.resize(s + n, 0);
		&mut self.0[s..]
	}
}

/// GPU profile mark: at the end of the pipe the GPU writes its 64-bit clock counter to `addr` (BOTTOM_OF_PIPE_TS, DATA_SEL 3;
/// what radv's timestamp queries use on this hardware). No cache flush, so the mark itself changes little.
pub(crate) fn eop_clock(addr: u64) -> [u32; 5] {
	[0x28 | 5 << 8, addr as u32, ((addr >> 32) as u32 & 0xffff) | 3 << 29, 0, 0]
}

/// End of pipe: flush and invalidate the colour and depth caches, then write the 64-bit `value` to `addr`.
pub(crate) fn eop_label64(c: &mut pm4::Cmd<'_>, addr: u64, value: u64) {
	// CACHE_FLUSH_AND_INV_TS_EVENT, EVENT_INDEX 5; DATA_SEL 2 = 64-bit data.
	c.pkt3(op::EVENT_WRITE_EOP, &[0x14 | 5 << 8, addr as u32, ((addr >> 32) as u32 & 0xffff) | 2 << 29, value as u32, (value >> 32) as u32]);
}

// ---------------------------------------------------------------------------------------------------------------------
// Pipelines.

/// A graphics pipeline found in the shader cache.
pub struct Gfx {
	pub key: u64,
	label: String,
	/// "vs entry/fs entry" (frame trace: picks out the overlay pipelines, `vs_hud*`, in any pass).
	entries: String,
	vs: Shader<'static>,
	ps: Option<Shader<'static>>,
	code: Vec<Alloc>,
	/// Everything the pipeline sets, ready to append: shader registers and the fixed state.
	pm4: Vec<u32>,
	/// Per vertex attribute (radv's GFX7 shaders take one descriptor per attribute, indexed by shader location):
	/// (location, buffer slot, byte offset in the vertex, format size, buffer stride).
	attrs: Vec<(u32, usize, u32, u32, u32)>,
	/// Stencil (read mask, write mask) front and back, for DB_STENCILREFMASK with the reference.
	stencil: Option<[(u32, u32); 2]>,
	/// Base of each group's dynamic buffers in radv's pipeline-wide numbering.
	dyn_base: Vec<u32>,
	dyn_total: u32,
	immediate_size: u32,
	restart: Option<wgt::IndexFormat>,
}

impl core::fmt::Debug for Gfx {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		write!(f, "Gfx({:016x} {})", self.key, self.label)
	}
}

fn stage_key(stage: &crate::ProgrammableStage<'_, super::ShaderModule>) -> Option<u64> {
	let n = stage.module.naga.as_ref()?;
	Some(crate::ps4key::stage(&n.module, stage.entry_point, stage.constants, stage.zero_initialize_workgroup_memory))
}

/// The 32-byte footer Sony's shader compiler puts after each shader ("OrbShdr", the code length, a hash). The GPU never reads
/// it, but the shadPS4 emulator (`sh ps4/emu-run.sh`, the PC check before a console test) finds a shader's length and identity
/// only through it and stops at the first draw without it. Layout as shadPS4's `AmdGpu::BinaryInfo`: signature[7], version,
/// a u32 with the length in bits 8..32, four bytes of usage info (zero: none), the u64 hash at 16, a CRC at 24 (unused, zero).
fn orbshdr_footer(code: &[u8]) -> [u8; 32] {
	let mut f = [0u8; 32];
	f[..7].copy_from_slice(b"OrbShdr");
	f[8..12].copy_from_slice(&((code.len() as u32 & 0xff_ffff) << 8).to_le_bytes());
	// FNV-1a over the code: shadPS4 keys its translated-shader cache by this hash, so different code needs different hashes.
	let mut h: u64 = 0xcbf2_9ce4_8422_2325;
	for &b in code {
		h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
	}
	f[16..24].copy_from_slice(&h.to_le_bytes());
	f
}

fn upload_code(shared: &Shared, code: &[u8]) -> Option<Alloc> {
	// The footer must start on a dword: shadPS4 searches for it one u32 at a time.
	let body = (code.len() + 3) & !3;
	let footer = orbshdr_footer(code);
	let a = shared.alloc_main(body + footer.len(), 256).ok()?;
	// SAFETY: a fresh allocation of `body + 32` bytes, `body >= code.len()`.
	unsafe {
		core::ptr::write_bytes(a.ptr, 0, body);
		core::ptr::copy_nonoverlapping(code.as_ptr(), a.ptr, code.len());
		core::ptr::copy_nonoverlapping(footer.as_ptr(), a.ptr.add(body), footer.len());
	}
	Some(a)
}

fn blend_factor(f: wgt::BlendFactor) -> u32 {
	use wgt::BlendFactor as B;
	match f {
		B::Zero => 0,
		B::One => 1,
		B::Src => 2,
		B::OneMinusSrc => 3,
		B::SrcAlpha => 4,
		B::OneMinusSrcAlpha => 5,
		B::DstAlpha => 6,
		B::OneMinusDstAlpha => 7,
		B::Dst => 8,
		B::OneMinusDst => 9,
		B::SrcAlphaSaturated => 10,
		B::Constant => 13,
		B::OneMinusConstant => 14,
		B::Src1 => 15,
		B::OneMinusSrc1 => 16,
		B::Src1Alpha => 17,
		B::OneMinusSrc1Alpha => 18,
	}
}

fn blend_op(o: wgt::BlendOperation) -> u32 {
	use wgt::BlendOperation as O;
	match o {
		O::Add => 0,
		O::Subtract => 1,
		O::Min => 2,
		O::Max => 3,
		O::ReverseSubtract => 4,
	}
}

fn blend_control(b: &wgt::BlendState) -> u32 {
	let comp = |c: &wgt::BlendComponent| {
		// Min / max ignore the factors; the hardware wants ONE there.
		let minmax = matches!(c.operation, wgt::BlendOperation::Min | wgt::BlendOperation::Max);
		let (s, d) = if minmax { (1, 1) } else { (blend_factor(c.src_factor), blend_factor(c.dst_factor)) };
		(s, blend_op(c.operation), d)
	};
	let (cs, co, cd) = comp(&b.color);
	let (as_, ao, ad) = comp(&b.alpha);
	cs | co << 5 | cd << 8 | as_ << 16 | ao << 21 | ad << 24 | 1 << 29 | 1 << 30
}

fn stencil_op(o: wgt::StencilOperation) -> u32 {
	use wgt::StencilOperation as S;
	match o {
		S::Keep => 0,
		S::Zero => 1,
		S::Replace => 3,
		S::Invert => 7,
		S::IncrementClamp => 5,
		S::DecrementClamp => 6,
		S::IncrementWrap => 8,
		S::DecrementWrap => 9,
	}
}

fn prim_type(t: wgt::PrimitiveTopology) -> u32 {
	use wgt::PrimitiveTopology as P;
	match t {
		P::PointList => 1,
		P::LineList => 2,
		P::LineStrip => 3,
		P::TriangleList => 4,
		P::TriangleStrip => 6,
	}
}

/// The do-nothing pixel shader's registers (sceGnmSetPsShader350 order): no exports, one interpolant enabled (the
/// hardware requires one).
fn noop_ps_regs(addr: u64) -> [u32; 12] {
	[(addr >> 8) as u32, (addr >> 40) as u32, 0, 0, 0, 0, 2, 2, 0, 0, 0, 0]
}

fn dyn_bases(layout: &PipelineLayout) -> (Vec<u32>, u32) {
	let mut base = Vec::new();
	let mut total = 0;
	for &n in &layout.dynamic_counts {
		base.push(total);
		total += n;
	}
	(base, total)
}

impl Gfx {
	pub fn new(shared: &Shared, desc: &crate::RenderPipelineDescriptor<PipelineLayout, super::ShaderModule, Resource>) -> Option<Gfx> {
		let label = desc.label.unwrap_or("-");
		let crate::VertexProcessor::Standard { vertex_buffers, vertex_stage } = &desc.vertex_processor else {
			shared.once("mesh", || String::from("mesh shaders are not supported"));
			return None;
		};
		let mut stage_keys = vec![("vs", stage_key(vertex_stage)?)];
		if let Some(fs) = &desc.fragment_stage {
			stage_keys.push(("fs", stage_key(fs)?));
		}
		let key = crate::ps4key::render(
			desc.layout.key,
			&stage_keys,
			&crate::ps4key::RenderState {
				vertex_buffers: vertex_buffers,
				primitive: &desc.primitive,
				depth_stencil: &desc.depth_stencil,
				multisample: &desc.multisample,
				color_targets: desc.color_targets,
				multiview_mask: desc.multiview_mask,
			},
		);
		let mut stages = shared.cache.stages(key, desc.label)?.into_iter();
		let vs = stages.next().filter(|s| s.stage == Stage::Vertex);
		let ps = stages.next();
		let Some(vs) = vs else {
			log::error!("{}", super::pipe::mismatch(key, "first stage is not a vertex shader"));
			return None;
		};
		if desc.fragment_stage.is_some() != ps.is_some() || ps.as_ref().is_some_and(|p| p.stage != Stage::Fragment) {
			log::error!("{}", super::pipe::mismatch(key, "stages do not match the descriptor"));
			return None;
		}
		// Scratch (register spills, big local arrays): the shared ring, or the pipeline is treated as missing without it.
		let scratch = vs.scratch_bytes_per_wave.max(ps.as_ref().map_or(0, |p| p.scratch_bytes_per_wave));
		let tmpring = match shared.scratch_tmpring(scratch, key, desc.label) {
			Some(t) => t,
			None => return None,
		};
		let vs_code = upload_code(shared, vs.code)?;
		let ps_code = match &ps {
			Some(p) => Some(upload_code(shared, p.code)?),
			None => None,
		};
		let vs_regs = vs.vs_regs(vs_code.addr());
		let ps_regs = match (&ps, ps_code) {
			(Some(p), Some(c)) => p.ps_regs(c.addr()),
			_ => noop_ps_regs(shared.noop_ps.addr()),
		};

		let mut words = Vec::with_capacity(256);
		let mut w = W(&mut words);
		plat::set_vs(w.reserve(29), &vs_regs);
		plat::set_ps(w.reserve(40), &ps_regs);
		w.sh(regs::SPI_SHADER_PGM_RSRC3_PS, &[0xffff]);
		w.sh(regs::SPI_SHADER_PGM_RSRC3_VS, &[0xffff, 0]);
		if let Some(p) = &ps {
			// radv's input words (FLAT_SHADE on flat inputs) when the cache has the masks; RAC_GNM_FLAT_SHADE=0 = the old words.
			static FAITHFUL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
			let faithful = *FAITHFUL.get_or_init(|| std::env::var("RAC_GNM_FLAT_SHADE").map_or(true, |v| v != "0"));
			let cntl = p.ps_input_cntl_with(&vs, faithful);
			if !cntl.is_empty() {
				w.ctx(regs::SPI_PS_INPUT_CNTL_0, &cntl);
			}
		}
		w.ctx(regs::VGT_SHADER_STAGES_EN, &[0]);
		// radv's GFX7 vertex shaders read the instance index from VGPR1, which the hardware loads as InstanceID / StepRate0;
		// radv's preamble sets the step rate to 1 once (ac_cmdbuf.c) and relies on it. Its reset value is 0, which collapsed
		// every per-instance fetch onto the first instance: the modern UI's glyphs (one instance each) all drew as one quad, so
		// the title menu's text was invisible (round 14 run 1 vs run 2's classic menu).
		w.ctx(VGT_INSTANCE_STEP_RATE_0, &[1, 1]);
		w.ctx(SPI_TMPRING_SIZE, &[tmpring]);

		// Colour targets: write masks, blending.
		let mut target_mask = 0u32;
		let mut blends = [0u32; 8];
		for (i, t) in desc.color_targets.iter().enumerate().take(8) {
			if let Some(t) = t {
				target_mask |= (t.write_mask.bits() & 0xf) << (4 * i);
				if let Some(b) = &t.blend {
					blends[i] = blend_control(b);
				}
			}
		}
		w.ctx(regs::CB_TARGET_MASK, &[target_mask]);
		w.ctx(regs::CB_COLOR_CONTROL, &[if target_mask != 0 { 1 << 4 | 0xcc << 16 } else { 0 }]);
		w.ctx(regs::CB_BLEND0_CONTROL, &blends);

		// Depth and stencil.
		let mut stencil = None;
		let (mut depth_control, mut stencil_control) = (0u32, 0u32);
		if let Some(ds) = &desc.depth_stencil {
			if ds.is_depth_enabled() {
				let write = ds.depth_write_enabled.unwrap_or(false);
				depth_control |= 1 << 1 | (write as u32) << 2 | super::compare(ds.depth_compare.unwrap_or(wgt::CompareFunction::Always)) << 4;
			}
			if ds.stencil.is_enabled() {
				let (f, b) = (&ds.stencil.front, &ds.stencil.back);
				depth_control |= 1 | 1 << 7 | super::compare(f.compare) << 8 | super::compare(b.compare) << 20;
				stencil_control = stencil_op(f.fail_op)
					| stencil_op(f.pass_op) << 4
					| stencil_op(f.depth_fail_op) << 8
					| stencil_op(b.fail_op) << 12
					| stencil_op(b.pass_op) << 16
					| stencil_op(b.depth_fail_op) << 20;
				stencil = Some([(ds.stencil.read_mask & 0xff, ds.stencil.write_mask & 0xff); 2]);
			}
			if ds.bias.is_enabled() {
				// Z32F: the offset scale counts float mantissa bits (POLY_OFFSET_NEG_NUM_DB_BITS = -23, DB_IS_FLOAT_FMT).
				w.ctx(PA_SU_POLY_OFFSET_DB_FMT_CNTL, &[(-23i32 as u32 & 0xff) | 1 << 8]);
				let scale = (ds.bias.slope_scale * 16.0).to_bits();
				let offset = (ds.bias.constant as f32).to_bits();
				w.ctx(PA_SU_POLY_OFFSET_CLAMP, &[ds.bias.clamp.to_bits(), scale, offset, scale, offset]);
			}
		}
		w.ctx(regs::DB_DEPTH_CONTROL, &[depth_control]);
		w.ctx(DB_STENCIL_CONTROL, &[stencil_control]);

		// Rasterizer.
		let p = &desc.primitive;
		let cull = match p.cull_mode {
			Some(wgt::Face::Front) => 1,
			Some(wgt::Face::Back) => 2,
			None => 0,
		};
		let cw = (p.front_face == wgt::FrontFace::Cw) as u32;
		let poly = match p.polygon_mode {
			wgt::PolygonMode::Fill => 0,
			wgt::PolygonMode::Line => 1 << 3 | 1 << 5 | 1 << 8,
			wgt::PolygonMode::Point => 1 << 3,
		};
		let bias_on = desc.depth_stencil.as_ref().is_some_and(|d| d.bias.is_enabled()) as u32;
		w.ctx(regs::PA_SU_SC_MODE_CNTL, &[cull | cw << 2 | poly | bias_on << 11 | bias_on << 12]);
		// DX clip space (z in 0..w), linear attribute clipping; unclipped depth turns the near / far planes off.
		let clip = vs.vs_clip_cull_pointsize & 0x3f;
		let unclipped = if p.unclipped_depth { 1 << 26 | 1 << 27 } else { 0 };
		w.ctx(regs::PA_CL_CLIP_CNTL, &[clip | 1 << 19 | 1 << 24 | unclipped]);
		w.uconfig(regs::VGT_PRIMITIVE_TYPE, &[prim_type(p.topology)]);
		w.ctx(VGT_MULTI_PRIM_IB_RESET_EN, &[p.strip_index_format.is_some() as u32]);
		if let Some(f) = p.strip_index_format {
			w.ctx(VGT_MULTI_PRIM_IB_RESET_INDX, &[if f == wgt::IndexFormat::Uint16 { 0xffff } else { 0xffff_ffff }]);
		}
		if desc.multisample.count > 1 {
			shared.once("msaa-pipe", || String::from("multisampled pipelines draw single-sampled"));
		}

		let mut attrs = Vec::new();
		for (slot, v) in vertex_buffers.iter().enumerate() {
			if let Some(v) = v {
				for a in v.attributes {
					attrs.push((a.shader_location, slot, a.offset as u32, a.format.size() as u32, v.array_stride as u32));
				}
			}
		}
		let (dyn_base, dyn_total) = dyn_bases(desc.layout);
		let mut code = vec![vs_code];
		code.extend(ps_code);
		let entries = format!("{}/{}", vertex_stage.entry_point, desc.fragment_stage.as_ref().map_or("-", |f| f.entry_point));
		Some(Gfx {
			key,
			label: label.into(),
			entries,
			vs,
			ps,
			code,
			pm4: words,
			attrs,
			stencil,
			dyn_base,
			dyn_total,
			immediate_size: desc.layout.immediate_size,
			restart: p.strip_index_format,
		})
	}

	pub fn free(self, shared: &Shared) {
		for a in self.code {
			shared.main.free(a);
		}
	}
}

/// A compute pipeline found in the shader cache.
pub struct Comp {
	pub key: u64,
	cs: Shader<'static>,
	code: Alloc,
	pm4: Vec<u32>,
	dyn_base: Vec<u32>,
	dyn_total: u32,
	immediate_size: u32,
}

impl core::fmt::Debug for Comp {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		write!(f, "Comp({:016x})", self.key)
	}
}

impl Comp {
	pub fn new(shared: &Shared, desc: &crate::ComputePipelineDescriptor<PipelineLayout, super::ShaderModule, Resource>) -> Option<Comp> {
		let key = crate::ps4key::compute(desc.layout.key, stage_key(&desc.stage)?);
		let cs = shared.cache.stages(key, desc.label)?.into_iter().next().filter(|s| s.stage == Stage::Compute);
		let Some(cs) = cs else {
			log::error!("{}", super::pipe::mismatch(key, "not a compute shader"));
			return None;
		};
		let tmpring = shared.scratch_tmpring(cs.scratch_bytes_per_wave, key, desc.label)?;
		let code = upload_code(shared, cs.code)?;
		let mut words = Vec::new();
		let mut w = W(&mut words);
		let a = code.addr();
		w.sh(COMPUTE_PGM_LO, &[(a >> 8) as u32, (a >> 40) as u32]);
		w.sh(COMPUTE_PGM_RSRC1, &[cs.rsrc1, cs.rsrc2]);
		w.sh(COMPUTE_RESOURCE_LIMITS, &[0, 0xffff_ffff, 0xffff_ffff, tmpring]); // + STATIC_THREAD_MGMT_SE0/SE1, TMPRING_SIZE
		let _ = (COMPUTE_STATIC_THREAD_MGMT_SE0, COMPUTE_TMPRING_SIZE);
		w.sh(COMPUTE_STATIC_THREAD_MGMT_SE2, &[0xffff_ffff, 0xffff_ffff]);
		w.sh(COMPUTE_START_X, &[0, 0, 0]);
		w.sh(COMPUTE_NUM_THREAD_X, &[cs.cs_block[0].max(1), cs.cs_block[1].max(1), cs.cs_block[2].max(1)]);
		let (dyn_base, dyn_total) = dyn_bases(desc.layout);
		Some(Comp { key, cs, code, pm4: words, dyn_base, dyn_total, immediate_size: desc.layout.immediate_size })
	}

	pub fn free(self, shared: &Shared) {
		shared.main.free(self.code);
	}
}

// ---------------------------------------------------------------------------------------------------------------------
// Command buffers.

/// RAC_GNM_AB_VB=45-59:ones3;60-74:offs: the vertex-fetch override of the window `frame` falls in, if any (see `vertex_table`).
fn ab_vb(frame: u64) -> Option<&'static str> {
	static W: std::sync::OnceLock<Vec<(u64, u64, &'static str)>> = std::sync::OnceLock::new();
	let w = W.get_or_init(|| {
		std::env::var("RAC_GNM_AB_VB")
			.unwrap_or_default()
			.split(';')
			.filter_map(|win| {
				let (range, what) = win.split_once(':')?;
				let (a, b) = range.trim().split_once('-')?;
				let what: &'static str = match what.trim() {
					"ones3" => "ones3",
					"offs" => "offs",
					_ => return None,
				};
				Some((a.trim().parse().ok()?, b.trim().parse().ok()?, what))
			})
			.collect()
	});
	w.iter().find(|(a, b, _)| (*a..=*b).contains(&frame)).map(|w| w.2)
}

/// A recorded command buffer: PM4 words (copied into GPU memory at submit) and the low-heap uploads its packets point at.
pub struct CommandBuffer {
	pub(crate) words: Vec<u32>,
	uploads: Vec<Alloc>,
	/// Keys of the pipelines bound, in first-use order (for the GPU hang report).
	pub(crate) used: Vec<u64>,
	shared: Arc<Shared>,
}

impl core::fmt::Debug for CommandBuffer {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		write!(f, "CommandBuffer({} dwords)", self.words.len())
	}
}

impl Drop for CommandBuffer {
	fn drop(&mut self) {
		// wgpu-core drops command buffers once the GPU is done with them.
		for a in self.uploads.drain(..) {
			self.shared.low.free(a);
		}
	}
}

/// A bound bind group: its set address and its dynamic buffers with this binding's offsets applied.
#[derive(Clone, Default)]
struct Bound {
	addr: u32,
	dynamic: Vec<(u64, u32)>,
}

const UPLOAD_CHUNK: usize = 64 << 10;

pub struct CommandEncoder {
	shared: Arc<Shared>,
	words: Vec<u32>,
	uploads: Vec<Alloc>,
	/// The current upload chunk and the bytes used in it.
	chunk: Option<(Alloc, usize)>,
	/// Work recorded since the last barrier.
	dirty: bool,
	gfx: Option<Arc<Gfx>>,
	comp: Option<Arc<Comp>>,
	/// A pipeline is bound but missing from the cache: skip its draws / dispatches.
	skip: bool,
	groups: [Option<Bound>; 8],
	vbs: [Option<(u64, u64)>; 16],
	index: Option<(u64, u64, wgt::IndexFormat)>,
	immediates: [u32; 64],
	stencil_ref: u32,
	/// The current render pass's resolves, done at its end: (source, destination) as (address, pitch in texels, layer
	/// bytes, layers), plus texel bytes, width and height. Multisampled targets are drawn single-sampled, so a resolve
	/// is a plain copy.
	resolves: Vec<Resolve>,
	/// Keys of the pipelines bound since `begin_encoding` (first use each).
	used: Vec<u64>,
	/// Draws in the current render pass (frame trace).
	trace_draws: u32,
	/// The current pass's label and whether its group 0 buffers are queued for the dump (frame trace).
	trace_pass: String,
	/// The current render pass's label (blit check lines).
	blit_pass: String,
	/// GPU profile: the pass being recorded, when its frame is profiled (`Shared::gpu_prof_mark`).
	prof_pass: Option<String>,
	trace_dumped: bool,
	/// The traced pass's first colour target: (address, pitch, width, height, texel bytes, format, SCREEN mark).
	trace_target: Option<(u64, u32, u32, u32, u32, wgt::TextureFormat, &'static str)>,
}

/// One colour resolve of a render pass (see `CommandEncoder::resolves`).
#[derive(Clone, Copy, Debug)]
struct Resolve {
	src: (u64, u32, u64, u32),
	dst: (u64, u32, u64, u32),
	bytes: u32,
	width: u32,
	height: u32,
	/// `RAC_GNM_BLIT=resolve`: the destination view and the source's image descriptor, for a copy draw (blit.rs).
	blit: Option<(TextureView, [u32; 8])>,
}

impl core::fmt::Debug for CommandEncoder {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		write!(f, "CommandEncoder({} dwords)", self.words.len())
	}
}

impl CommandEncoder {
	pub fn new(shared: Arc<Shared>) -> Self {
		CommandEncoder {
			shared,
			words: Vec::new(),
			uploads: Vec::new(),
			chunk: None,
			dirty: false,
			gfx: None,
			comp: None,
			skip: false,
			groups: Default::default(),
			vbs: [None; 16],
			index: None,
			immediates: [0; 64],
			stencil_ref: 0,
			resolves: Vec::new(),
			used: Vec::new(),
			trace_draws: 0,
			trace_pass: String::new(),
			blit_pass: String::new(),
			prof_pass: None,
			trace_dumped: false,
			trace_target: None,
		}
	}

	fn w(&mut self) -> W<'_> {
		W(&mut self.words)
	}

	/// Copies `data` into low-heap memory for this command buffer; returns its GPU address.
	fn upload(&mut self, data: &[u32]) -> u64 {
		let bytes = (data.len() * 4).next_multiple_of(16).max(16);
		let fits = self.chunk.as_ref().is_some_and(|(a, used)| used + bytes <= a.len);
		if !fits {
			let size = bytes.max(UPLOAD_CHUNK);
			match self.shared.alloc_low(size, 256) {
				Ok(a) => {
					self.uploads.push(a);
					self.chunk = Some((a, 0));
				}
				Err(_) => return self.shared.zeros.addr(),
			}
		}
		let (a, used) = self.chunk.as_mut().unwrap();
		// SAFETY: inside the chunk, which only this encoder writes.
		let p = unsafe { a.ptr.add(*used) } as *mut u32;
		unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len()) };
		*used += bytes;
		p as u64
	}

	/// Drains the GPU: end-of-pipe flush of everything, wait for it, invalidate the shader caches (and write back the L2).
	fn barrier(&mut self) {
		let n = self.shared.barrier_count.fetch_add(1, core::sync::atomic::Ordering::Relaxed).wrapping_add(1);
		let a = self.shared.barrier_label.addr();
		let mut w = self.w();
		// CS_PARTIAL_FLUSH: compute work on this ring finishes too.
		w.pkt3(OP_EVENT_WRITE, &[7 | 4 << 8]);
		// CACHE_FLUSH_AND_INV_TS_EVENT, 32-bit data.
		w.pkt3(op::EVENT_WRITE_EOP, &[0x14 | 5 << 8, a as u32, ((a >> 32) as u32 & 0xffff) | 1 << 29, n, 0]);
		// WAIT_REG_MEM: memory, equal.
		w.pkt3(OP_WAIT_REG_MEM, &[3 | 1 << 4, a as u32, (a >> 32) as u32, n, 0xffff_ffff, 10]);
		// ACQUIRE_MEM: TC (L2 write back + invalidate on GFX7), TCL1, scalar and instruction caches.
		w.pkt3(op::ACQUIRE_MEM, &[1 << 23 | 1 << 22 | 1 << 27 | 1 << 29, 0xffff_ffff, 0x00ff_ffff, 0, 0, 10]);
		self.dirty = false;
	}

	fn barrier_if_dirty(&mut self) {
		if self.dirty {
			self.barrier();
		}
	}

	/// CP DMA: fill `bytes` at `dst` with the 32-bit `value` (src = None) or copy from `src`. Direct memory both sides.
	fn dma(&mut self, src: Option<u64>, value: u32, dst: u64, bytes: u64) {
		static DMA_L2: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
		self.shared.dma_bytes[src.is_some() as usize].fetch_add(bytes, core::sync::atomic::Ordering::Relaxed);
		// Frame trace: every CP DMA of 256 KB or more (`RAC_GNM_TRACE_DMA=bytes` sets the size; the GPU's slow path, see blit.rs),
		// with the pass it is recorded in.
		static MIN: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
		if bytes >= *MIN.get_or_init(|| std::env::var("RAC_GNM_TRACE_DMA").ok().and_then(|v| v.parse().ok()).unwrap_or(256 << 10)) {
			if let Some(n) = self.shared.tracing() {
				std::eprintln!("GNM trace {n}:   CP DMA {} {} KB {:x} -> {:x} (pass {})", if src.is_some() { "copy" } else { "fill" }, bytes >> 10, src.unwrap_or(0), dst, self.trace_pass);
			}
		}
		// `RAC_GNM_DMA_LOG=bytes`: every CP DMA of at least that size in any frame (to find the occasional big ones).
		static LOG: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
		let log_min = *LOG.get_or_init(|| std::env::var("RAC_GNM_DMA_LOG").ok().and_then(|v| v.parse().ok()).unwrap_or(u64::MAX));
		if bytes >= log_min {
			let n = self.shared.shown.load(core::sync::atomic::Ordering::Relaxed) + 1;
			std::eprintln!("GNM: frame {n}: CP DMA {} {} KB {:x} -> {:x} (pass {})", if src.is_some() { "copy" } else { "fill" }, bytes >> 10, src.unwrap_or(0), dst, self.trace_pass);
		}
		let mut done = 0;
		while done < bytes {
			let n = (bytes - done).min(DMA_MAX);
			let (sel, s) = match src {
				Some(s) => (0u32, s + done),
				None => (2u32, value as u64),
			};
			let d = dst + done;
			// SRC_SEL (2 = data), DST_SEL 0 (memory), CP_SYNC: the CP waits for the transfer before going on.
			// `RAC_GNM_DMA_L2=1`: source and destination through the L2 cache (SRC_SEL / DST_SEL 3, CIK's TC_L2), as radv and
			// radeonsi do on GFX7 (Mesa `cp_dma_use_L2`). The barrier before the colour / depth units or the display read
			// anything writes L2 back (ACQUIRE_MEM TC), and one after CB / DB writes invalidates it.
			let l2 = *DMA_L2.get_or_init(|| std::env::var("RAC_GNM_DMA_L2").is_ok_and(|v| v == "1"));
			let sel = if l2 && sel == 0 { 3 } else { sel };
			let header = sel << 29 | if l2 { 3 << 20 } else { 0 } | 1 << 31;
			let lo = if src.is_some() { s as u32 } else { value };
			let hi = if src.is_some() { (s >> 32) as u32 } else { 0 };
			self.w().pkt3(OP_DMA_DATA, &[header, lo, hi, d as u32, (d >> 32) as u32, n as u32]);
			done += n;
		}
		self.dirty = true;
	}

	/// Fills `bytes` at `dst` with `texel` repeated (any texel size dividing `FILL_BLOCK`): one block written by the CPU into
	/// the low heap, then CP DMA copies of it.
	fn fill_texels(&mut self, texel: &[u8], dst: u64, bytes: u64) {
		if texel.is_empty() || FILL_BLOCK % texel.len() != 0 {
			return self.dma(None, 0, dst, bytes);
		}
		let block: Vec<u32> = texel.iter().copied().cycle().take(FILL_BLOCK).collect::<Vec<u8>>().chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
		let src = self.upload(&block);
		let mut done = 0;
		while done < bytes {
			let n = (bytes - done).min(FILL_BLOCK as u64);
			self.dma(Some(src), 0, dst + done, n);
			done += n;
		}
	}

	/// The user SGPRs of one stage for the bound pipeline, written at `base` (the stage's USER_DATA_0 register).
	/// Runs for every draw (twice: VS and PS), so it builds everything on the stack: no heap allocation. The register writes
	/// and the uploads come in the same order as they always did, so the command words are unchanged.
	fn user_data(&mut self, shader_ix: u8, base: u32, extra: &[(usize, &[u32])]) {
		// A clone of the pipeline's Arc: the shader borrows it while uploads borrow self.
		let gfx = self.gfx.clone();
		let comp = self.comp.clone();
		let (shader, pipe_dyn_base, dyn_total, imm_size): (&Shader<'static>, &[u32], u32, u32) = match (shader_ix, &gfx, &comp) {
			(0, Some(g), _) => (&g.vs, &g.dyn_base, g.dyn_total, g.immediate_size),
			(1, Some(g), _) => match &g.ps {
				Some(p) => (p, &g.dyn_base, g.dyn_total, g.immediate_size),
				None => return,
			},
			(2, _, Some(c)) => (&c.cs, &c.dyn_base, c.dyn_total, c.immediate_size),
			_ => return,
		};
		if let Some(l) = shader.ud(ud::SCRATCH_RING_OFFSETS) {
			// The ring table (scratch base, swizzle bit) for shaders that spill; zeros for the others, which never read it.
			let z = if shader.scratch_bytes_per_wave > 0 { self.shared.scratch_table() } else { self.shared.zeros.addr() };
			let v = [z as u32, (z >> 32) as u32];
			let n = (l.count as usize).min(2);
			if n > 0 {
				self.w().sh(base + 4 * l.sgpr, &v[..n]);
			}
		}
		for set in 0..8 {
			if let Some(l) = shader.set_sgpr(set) {
				let addr = self.groups[set].as_ref().map_or(0, |b| b.addr);
				self.w().sh(base + 4 * l.sgpr, &[addr]);
			}
		}
		if let Some(l) = shader.ud(ud::PUSH_CONSTANTS) {
			let n = (imm_size as usize).div_ceil(4).max(1).min(64);
			let data = self.immediates;
			let a = self.upload(&data[..n]);
			self.w().sh(base + 4 * l.sgpr, &[a as u32]);
		}
		if let Some(l) = shader.ud(ud::INLINE_PUSH_CONSTANTS) {
			let mut v = [0u32; 64];
			let count = (l.count as usize).min(64);
			let mut k = 0;
			let mut m = shader.inline_push_constant_mask;
			while m != 0 && k < count {
				let b = m.trailing_zeros() as usize;
				m &= m - 1;
				v[k] = self.immediates.get(b).copied().unwrap_or(0);
				k += 1;
			}
			if count > 0 {
				self.w().sh(base + 4 * l.sgpr, &v[..count]);
			}
		}
		if let Some(l) = shader.ud(UD_DYNAMIC_DESCRIPTORS) {
			let len = dyn_total as usize * 4;
			let a = if len == 0 {
				self.shared.zeros.addr()
			} else {
				// Small tables on the stack (the usual case: a few dynamic buffers), larger ones on the heap.
				let mut stack = [0u32; 64];
				let mut heap = Vec::new();
				let table: &mut [u32] = if len <= stack.len() {
					&mut stack[..len]
				} else {
					heap.resize(len, 0);
					&mut heap
				};
				for (g, b) in self.groups.iter().enumerate() {
					let (Some(b), Some(&gbase)) = (b, pipe_dyn_base.get(g)) else { continue };
					for (i, &(addr, size)) in b.dynamic.iter().enumerate() {
						let at = (gbase as usize + i) * 4;
						if at + 4 <= table.len() {
							table[at..at + 4].copy_from_slice(&desc::buffer(addr, 0, size));
						}
					}
				}
				let t: &[u32] = table;
				self.upload(t)
			};
			self.w().sh(base + 4 * l.sgpr, &[a as u32]);
		}
		for &(slot, vals) in extra {
			if let Some(l) = shader.ud(slot) {
				let mut v = [0u32; 16];
				let count = (l.count as usize).min(16);
				let n = vals.len().min(count);
				v[..n].copy_from_slice(&vals[..n]);
				if count > 0 {
					self.w().sh(base + 4 * l.sgpr, &v[..count]);
				}
			}
		}
	}

	/// GPU profile: a mark before a pass (what the GPU did since the last pass ended) and the pass's name for `prof_end`.
	fn prof_begin(&mut self, label: crate::Label<'_>) {
		self.prof_pass = None;
		if let Some(addr) = self.shared.gpu_prof_mark("(between passes)") {
			self.w().pkt3(op::EVENT_WRITE_EOP, &eop_clock(addr));
			self.prof_pass = Some(String::from(label.unwrap_or("(no label)")));
		}
	}

	/// GPU profile: the mark that ends the pass begun by `prof_begin`.
	fn prof_end(&mut self) {
		if let Some(what) = self.prof_pass.take() {
			if let Some(addr) = self.shared.gpu_prof_mark(&what) {
				self.w().pkt3(op::EVENT_WRITE_EOP, &eop_clock(addr));
			}
		}
	}

	/// The vertex-buffer table for the bound graphics pipeline; returns its address. radv's GFX7 vertex shaders read one
	/// descriptor per attribute the shader really fetches (`vs_vb_desc_usage_mask` has a bit per used location), and the
	/// table is packed: a location's descriptor sits at the count of used locations below it (radv_nir_lower_vs_inputs.c
	/// `desc_index`, radv_write_vertex_descriptors), not at the location itself. Indexing by location left every shader that
	/// skips an attribute (terrain, sky, shrubs, the HUD: location 2 unused) reading a zeroed descriptor for its vertex
	/// colour, so it drew black on the console. Each descriptor: the buffer's address (the shader adds the attribute's offset itself: console run 2 read it twice), the buffer's stride,
	/// and the record count radv computes for GFX7 (whole vertices whose attribute fits: (size - offset - format size) /
	/// stride + 1; 1 for stride 0).
	fn vertex_table(&mut self) -> u64 {
		let Some(g) = self.gfx.clone() else { return 0 };
		let mask = g.vs.vs_vb_desc_usage_mask;
		if mask == 0 {
			return 0;
		}
		let n = mask.count_ones() as usize;
		let mut table = [0u32; 128];
		// RAC_GNM_AB_VB (console diagnostics): "ones3" points location 3 (the vertex colour) at a constant (1, 1, 1, 1); "offs" also
		// puts each attribute's offset into its descriptor's base, for frame windows (`ab_vb`).
		let ab = ab_vb(self.shared.shown.load(core::sync::atomic::Ordering::Relaxed) + 1);
		for &(loc, slot, offset, fsize, stride) in &g.attrs {
			if loc >= 32 || mask & (1 << loc) == 0 {
				continue;
			}
			let at = (mask & ((1u32 << loc) - 1)).count_ones() as usize * 4;
			if ab == Some("ones3") && loc == 3 {
				let ones = self.upload(&[1.0f32.to_bits(); 4]);
				table[at..at + 4].copy_from_slice(&desc::buffer(ones, 0, 1));
				continue;
			}
			let Some(Some((addr, size))) = self.vbs.get(slot).copied() else { continue };
			let (addr, size) = if ab == Some("offs") { (addr + offset as u64, size.saturating_sub(offset as u64)) } else { (addr, size) };
			let end = (offset + fsize) as u64;
			let records = if size < end {
				0
			} else if stride == 0 {
				1
			} else {
				((size - end) / stride as u64 + 1) as u32
			};
			table[at..at + 4].copy_from_slice(&desc::buffer(addr, stride.min((1 << 14) - 1), records));
		}
		self.upload(&table[..n * 4])
	}

	fn graphics_user_data(&mut self, base_vertex: i32, first_instance: u32) {
		let vt = self.vertex_table();
		let vt = [vt as u32];
		let bv = [base_vertex as u32, first_instance];
		self.user_data(0, regs::SPI_SHADER_USER_DATA_VS_0, &[(ud::VS_VERTEX_BUFFERS, &vt), (ud::VS_BASE_VERTEX_START_INSTANCE, &bv)]);
		self.user_data(1, regs::SPI_SHADER_USER_DATA_PS_0, &[]);
	}

	/// `draw_count` indirect draws whose arguments start at `offset` in `buffer` (wgpu's layouts: 16 bytes {vertex count,
	/// instances, first vertex, first instance}, indexed 20 bytes {index count, instances, first index, base vertex, first
	/// instance}). Like radv on GFX7: SET_BASE points the CP at the buffer, and each DRAW_(INDEX_)INDIRECT names the VS
	/// user-data registers the CP writes the base vertex and first instance into (AC_UD_VS_BASE_VERTEX_START_INSTANCE), so
	/// the shaders see them exactly as for a direct draw. One packet per draw instead of DRAW_*_INDIRECT_MULTI: the single
	/// form is what GNM itself exposes, so the console's CP firmware surely has it.
	fn indirect(&mut self, buffer: &Buffer, offset: u64, draw_count: u32, indexed: bool) {
		if self.skip || draw_count == 0 {
			if self.skip {
				self.shared.skipped.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
			}
			return;
		}
		let Some(g) = self.gfx.clone() else { return };
		let index = if indexed {
			match self.index {
				Some(i) => Some(i),
				None => return,
			}
		} else {
			None
		};
		// Vertex buffers, sets and immediates as for a direct draw; the base vertex / first instance SGPRs are then
		// overwritten by the CP from each draw's arguments.
		self.graphics_user_data(0, 0);
		// SH register indices (from 0xb000) for the CP. A VS without the slot gets user-data register 15, which no shader
		// with fewer user SGPRs reads, so the CP's write lands somewhere harmless.
		let spare = (regs::SPI_SHADER_USER_DATA_VS_0 + 4 * 15 - 0xb000) / 4;
		let (vtx_reg, inst_reg) = match g.vs.ud(ud::VS_BASE_VERTEX_START_INSTANCE) {
			Some(l) => {
				let r = (regs::SPI_SHADER_USER_DATA_VS_0 + 4 * l.sgpr - 0xb000) / 4;
				(r, if l.count >= 2 { r + 1 } else { 0 })
			}
			None => (spare, 0),
		};
		let base = buffer.addr();
		let stride = if indexed { 20 } else { 16 };
		let mut w = self.w();
		w.pkt3(OP_SET_BASE, &[1, base as u32, (base >> 32) as u32]);
		if let Some((addr, size, format)) = index {
			let isize = if format == wgt::IndexFormat::Uint16 { 2 } else { 4 };
			w.pkt3(op::INDEX_TYPE, &[(isize == 4) as u32]);
			w.pkt3(OP_INDEX_BASE, &[addr as u32, (addr >> 32) as u32 & 0xffff]);
			w.pkt3(OP_INDEX_BUFFER_SIZE, &[(size / isize) as u32]);
		}
		for i in 0..draw_count as u64 {
			let at = (offset + i * stride) as u32;
			// DI_SRC_SEL: 0 = DMA (index buffer), 2 = auto index.
			let (opc, src) = if indexed { (OP_DRAW_INDEX_INDIRECT, 0) } else { (OP_DRAW_INDIRECT, 2) };
			w.pkt3(opc, &[at, vtx_reg, inst_reg, src]);
		}
		self.dirty = true;
	}

	fn stencil_refmask(&mut self) {
		if let Some(s) = self.gfx.as_ref().and_then(|g| g.stencil) {
			let r = self.stencil_ref & 0xff;
			let v = |(rm, wm): (u32, u32)| r | rm << 8 | wm << 16 | 1 << 24;
			let (f, b) = (v(s[0]), v(s[1]));
			self.w().ctx(DB_STENCILREFMASK, &[f, b]);
		}
	}

	/// Viewport registers (the Vulkan back end's convention: Y flipped by a negative height, depth range as given).
	fn viewport(&mut self, r: &crate::Rect<f32>, depth: core::ops::Range<f32>) {
		let (hw, hh) = (r.w / 2.0, r.h / 2.0);
		let vals = [hw.to_bits(), (r.x + hw).to_bits(), (-hh).to_bits(), (r.y + hh).to_bits(), (depth.end - depth.start).to_bits(), depth.start.to_bits()];
		let mut w = self.w();
		w.ctx(regs::PA_CL_VPORT_XSCALE, &vals);
		w.ctx(PA_SC_VPORT_ZMIN_0, &[depth.start.min(depth.end).to_bits(), depth.start.max(depth.end).to_bits()]);
		let (x0, y0) = (r.x.max(0.0) as u32, r.y.max(0.0) as u32);
		let (x1, y1) = ((r.x + r.w).max(0.0) as u32, (r.y + r.h).max(0.0) as u32);
		w.ctx(regs::PA_SC_VPORT_SCISSOR_0_TL, &[x0 | y0 << 16 | 1 << 31, x1.min(16384) | y1.min(16384) << 16]);
		w.ctx(regs::PA_CL_VTE_CNTL, &[0x43f]);
	}

	fn scissor(&mut self, r: &crate::Rect<u32>) {
		let (x1, y1) = ((r.x + r.w).min(16384), (r.y + r.h).min(16384));
		self.w().ctx(regs::PA_SC_GENERIC_SCISSOR_TL, &[r.x | r.y << 16 | 1 << 31, x1 | y1 << 16]);
	}

	fn color_target(&mut self, i: u32, v: Option<&TextureView>) {
		let reg = regs::CB_COLOR0_BASE + i * CB_COLOR_STRIDE;
		let Some(v) = v else {
			// COLOR_INVALID: the slot draws nothing.
			self.w().ctx(reg + 0x10, &[0]);
			return;
		};
		let f = v.fmt;
		let lo = (v.addr >> 8) as u32;
		let pitch = v.level.pitch;
		let slice = (pitch * v.level.height.next_multiple_of(8) / 64).max(1) - 1;
		// Unorm / snorm / sRGB clamp blending to 0..1; integer formats bypass blending.
		let int = matches!(f.cb_number, 4 | 5);
		let info = f.cb_format << 2 | f.cb_number << 8 | f.cb_swap << 11 | ((!int && f.cb_number != 7) as u32) << 15 | (int as u32) << 16;
		let tile = desc::TILE_DISPLAY_LINEAR_ALIGNED;
		self.w().ctx(reg, &[lo, pitch / 8 - 1, slice, 0, info, tile | tile << 5, 0, lo, 0, lo, slice, 0, 0]);
	}

	fn depth_target(&mut self, v: Option<&TextureView>) {
		let Some(v) = v else {
			let mut w = self.w();
			w.ctx(regs::DB_Z_INFO, &[0, 0]);
			w.ctx(regs::DB_RENDER_CONTROL, &[0]);
			return;
		};
		let Some((zfmt, stencil)) = v.fmt.depth else { return };
		let (pw, ph) = (v.level.pitch.div_ceil(8), v.level.height.div_ceil(8));
		let a = (v.addr >> 8) as u32;
		let s = (v.stencil_addr >> 8) as u32;
		let tile = desc::TILE_DEPTH_1D_THIN;
		let mut w = self.w();
		// DB_DEPTH_INFO: ARRAY_MODE 1D thin, PIPE_CONFIG P8_32x32_16x16 (GFX7 reads the layout here, not the tile index).
		w.ctx(regs::DB_DEPTH_INFO, &[
			2 << 4 | 12 << 8,
			zfmt | tile << 20,
			if stencil { 1 | tile << 20 } else { 0 },
			a,
			if stencil { s } else { a },
			a,
			if stencil { s } else { a },
			(pw - 1) | (ph - 1) << 11,
			pw * ph - 1,
		]);
		w.ctx(regs::DB_DEPTH_VIEW, &[0]);
		w.ctx(regs::DB_HTILE_DATA_BASE, &[0]);
		w.ctx(regs::DB_RENDER_CONTROL, &[0]);
	}
}

impl CommandEncoder {
	/// `RAC_GNM_BLIT_CHECK`: whether the frame being recorded checks its blits.
	fn blit_checking(&self) -> bool {
		blit::checking(self.shared.shown.load(core::sync::atomic::Ordering::Relaxed) + 1)
	}

	/// One blit draw (blit.rs) into the targets bound now: the blit vertex shader with `z`, pixel shader `ps` with user data
	/// `ps_user`, no blending, no culling, depth written with compare Always when `depth` (colour writes off then), over a
	/// `w` x `h` viewport. Leaves no pipeline bound: the next draw must set one (all of whose state it writes).
	fn blit_draw(&mut self, ps: &[u32; 12], z: f32, ps_user: &[u32], ds: Option<(bool, Option<u8>)>, w: u32, h: u32) {
		let depth = ds.is_some();
		let vs = blit::vs_regs(self.shared.blit_code() + blit::VS_AT as u64);
		{
			let mut wr = self.w();
			plat::set_vs(wr.reserve(29), &vs);
			plat::set_ps(wr.reserve(40), ps);
			wr.sh(regs::SPI_SHADER_PGM_RSRC3_PS, &[0xffff]);
			wr.sh(regs::SPI_SHADER_PGM_RSRC3_VS, &[0xffff, 0]);
			wr.ctx(regs::VGT_SHADER_STAGES_EN, &[0]);
			wr.ctx(regs::CB_TARGET_MASK, &[if depth { 0 } else { 0xf }]);
			wr.ctx(regs::CB_COLOR_CONTROL, &[if depth { 0 } else { 1 << 4 | 0xcc << 16 }]);
			wr.ctx(regs::CB_BLEND0_CONTROL, &[0; 8]);
			// Depth: Z_ENABLE + Z_WRITE with compare Always. Stencil: STENCIL_ENABLE, compare Always, Replace on pass (front
			// faces only: BACKFACE_ENABLE off), reference = the value, both masks 0xff.
			let always = super::compare(wgt::CompareFunction::Always);
			let (write_z, stencil) = ds.unwrap_or((false, None));
			let mut dc = if write_z { 1 << 1 | 1 << 2 | always << 4 } else { 0 };
			let mut sc = 0;
			if let Some(s) = stencil {
				dc |= 1 | always << 8;
				sc = 3 << 4;
				wr.ctx(DB_STENCILREFMASK, &[s as u32 | 0xff << 8 | 0xff << 16 | 1 << 24, s as u32 | 0xff << 8 | 0xff << 16 | 1 << 24]);
			}
			wr.ctx(regs::DB_DEPTH_CONTROL, &[dc]);
			wr.ctx(DB_STENCIL_CONTROL, &[sc]);
			wr.ctx(regs::PA_SU_SC_MODE_CNTL, &[0]);
			wr.ctx(regs::PA_CL_CLIP_CNTL, &[1 << 19 | 1 << 24]);
			wr.uconfig(regs::VGT_PRIMITIVE_TYPE, &[4]);
			wr.sh(regs::SPI_SHADER_USER_DATA_VS_0, &[z.to_bits()]);
			if !ps_user.is_empty() {
				wr.sh(regs::SPI_SHADER_USER_DATA_PS_0, ps_user);
			}
		}
		self.viewport(&crate::Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 }, 0.0..1.0);
		self.scissor(&crate::Rect { x: 0, y: 0, w, h });
		self.w().pkt3(op::NUM_INSTANCES, &[1]);
		plat::draw_auto(self.w().reserve(7), 3);
		self.gfx = None;
		self.dirty = true;
	}

	/// Render-pass clears by draws (`RAC_GNM_BLIT=clear`): each colour target in slot 0 alone, then the depth plane with no
	/// colour target. The caller binds the pass's real targets afterwards.
	fn blit_clears(&mut self, colour: &[(TextureView, wgt::Color)], depth: Option<(TextureView, Option<f32>, Option<u8>)>, w: u32, h: u32) {
		let br = h << 16 | w;
		{
			let mut wr = self.w();
			wr.ctx(regs::PA_SC_SCREEN_SCISSOR_TL, &[0, br]);
			wr.ctx(regs::PA_SC_WINDOW_OFFSET, &[0, 1 << 31, br]);
			wr.ctx(regs::PA_SC_AA_CONFIG, &[0]);
			wr.ctx(regs::PA_SC_AA_MASK_X0Y0_X1Y0, &[0xffff_ffff, 0xffff_ffff]);
		}
		for i in 1..8 {
			self.color_target(i, None);
		}
		self.depth_target(None);
		let check = self.blit_checking();
		let fill = blit::fill_regs(self.shared.blit_code() + blit::FILL_AT as u64);
		for (v, c) in colour {
			self.color_target(0, Some(v));
			let user = [c.r as f32, c.g as f32, c.b as f32, c.a as f32].map(f32::to_bits);
			self.blit_draw(&fill, 0.0, &user, None, v.level.width, v.level.height);
			if check {
				let want = clear_texel(&v.fmt, v.format, *c);
				self.blit_check_fill(v, v.addr, v.level.pitch as u64 * v.fmt.bytes as u64, v.fmt.bytes, want, "colour clear");
			}
		}
		if let Some((v, zv, sv)) = depth {
			self.color_target(0, None);
			self.depth_target(Some(&v));
			let ps = noop_ps_regs(self.shared.noop_ps.addr());
			self.blit_draw(&ps, zv.unwrap_or(0.0), &[], Some((zv.is_some(), sv)), v.level.width, v.level.height);
			if let (true, Some(s)) = (check, sv) {
				self.blit_check_fill(&v, v.stencil_addr, v.level.pitch as u64, 1, Some(alloc::vec![s]), "stencil clear");
			}
			if let (true, Some(z)) = (check, zv) {
				let want = if v.fmt.depth == Some((1, false)) {
					let z = (z * 65535.0).round() as u16;
					z.to_le_bytes().to_vec()
				} else {
					z.to_le_bytes().to_vec()
				};
				// The depth plane is tiled (8 x 8 blocks), but a uniform fill reads the same anywhere.
				let bpp = if v.fmt.depth == Some((1, false)) { 2 } else { 4 };
				self.blit_check_fill(&v, v.addr, v.level.pitch as u64 * bpp as u64, bpp, Some(want), "depth clear");
			}
		}
	}

	/// A large buffer clear (`RAC_GNM_BLIT=clear`; wgpu's buffer clears are zero fills): the 256-byte aligned middle drawn as a
	/// linear R32Float colour target `BUFFER_FILL_WIDTH` texels wide (whole rows), the ends by CP DMA. The volumetric fog's
	/// per-pixel buffer (volfog/soft.rs, 8 MB at 1920x1080) is emptied this way every Enhanced frame.
	fn blit_fill_buffer(&mut self, addr: u64, bytes: u64) {
		let row = blit::BUFFER_FILL_WIDTH as u64 * 4;
		let start = addr.next_multiple_of(256);
		let rows = (addr + bytes).saturating_sub(start) / row;
		if rows == 0 {
			return self.dma(None, 0, addr, bytes);
		}
		let end = start + rows * row;
		if start > addr {
			self.dma(None, 0, addr, start - addr);
		}
		if addr + bytes > end {
			self.dma(None, 0, end, addr + bytes - end);
		}
		let Some(fmt) = super::format::of(wgt::TextureFormat::R32Float) else { return self.dma(None, 0, start, end - start) };
		let level = super::Level { offset: 0, pitch: blit::BUFFER_FILL_WIDTH, width: blit::BUFFER_FILL_WIDTH, height: rows as u32, layer_bytes: rows * row, stencil_offset: 0, stencil_layer_bytes: 0 };
		let v = TextureView { addr: start, stencil_addr: 0, format: wgt::TextureFormat::R32Float, fmt, level, layers: 1, image: [0; 8], depth_aspect_only: false };
		let (w, h) = (level.width, level.height);
		{
			let mut wr = self.w();
			wr.ctx(regs::PA_SC_SCREEN_SCISSOR_TL, &[0, h << 16 | w]);
			wr.ctx(regs::PA_SC_WINDOW_OFFSET, &[0, 1 << 31, h << 16 | w]);
			wr.ctx(regs::PA_SC_AA_CONFIG, &[0]);
			wr.ctx(regs::PA_SC_AA_MASK_X0Y0_X1Y0, &[0xffff_ffff, 0xffff_ffff]);
		}
		for i in 1..8 {
			self.color_target(i, None);
		}
		self.depth_target(None);
		self.color_target(0, Some(&v));
		let fill = blit::fill_regs(self.shared.blit_code() + blit::FILL_AT as u64);
		self.blit_draw(&fill, 0.0, &[0; 4], None, w, h);
		if self.blit_checking() {
			self.blit_check_fill(&v, v.addr, row, 4, Some(alloc::vec![0; 4]), "buffer clear");
		}
	}

	/// `RAC_GNM_BLIT_CHECK`: after a clear draw, the middle row of the target read back (barrier, CP DMA) for comparison
	/// with the clear texel at present.
	fn blit_check_fill(&mut self, v: &TextureView, base: u64, row_pitch: u64, bpp: u32, want: Option<Vec<u8>>, kind: &str) {
		let Some(want) = want else { return };
		self.barrier();
		let row = v.level.width as u64 * bpp as u64;
		let Some(slot) = self.shared.snap_raw(row as usize) else { return };
		self.dma(Some(base + (v.level.height as u64 / 2) * row_pitch), 0, slot, row);
		let what = format!("{kind} {}x{} {:?} in pass {}", v.level.width, v.level.height, v.format, self.blit_pass);
		self.shared.blit_checks.lock().push(blit::Check { what, got: slot, want: blit::Want::Texel(want), bytes: row as usize });
	}
}

/// radv's AC_UD_DYNAMIC_DESCRIPTORS (Mesa 26.1.6): the pointer to the dynamic buffers' descriptors.
const UD_DYNAMIC_DESCRIPTORS: usize = 18;

/// A clear colour as a repeating 32-bit pattern; `None` if the texel is 8 or 16 bytes and does not repeat every 4 bytes
/// (or the format is unknown).
fn clear_pattern(f: &Fmt, format: wgt::TextureFormat, c: wgt::Color) -> Option<u32> {
	let bytes = clear_texel(f, format, c)?;
	// Repeat to 4 bytes, then check that longer texels repeat every 4.
	let mut b = bytes.clone();
	while b.len() < 4 {
		b.extend_from_slice(&bytes);
	}
	let w = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
	if b.chunks(4).all(|c| c.len() == 4 && u32::from_le_bytes([c[0], c[1], c[2], c[3]]) == w) { Some(w) } else { None }
}

/// A clear colour as one texel's bytes; `None` for a format this does not know (unless the colour is all zero).
fn clear_texel(f: &Fmt, format: wgt::TextureFormat, c: wgt::Color) -> Option<Vec<u8>> {
	let ch = [c.r, c.g, c.b, c.a];
	let unorm = |v: f64, bits: u32| ((v.clamp(0.0, 1.0) * ((1u64 << bits) - 1) as f64).round()) as u32;
	let srgb = |v: f64| {
		let v = v.clamp(0.0, 1.0);
		if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
	};
	let half = |v: f64| half_bits(v as f32) as u32;
	use wgt::TextureFormat as T;
	let bytes: Vec<u8> = match format {
		T::Rgba8Unorm => ch.iter().map(|&v| unorm(v, 8) as u8).collect(),
		T::Rgba8UnormSrgb => [srgb(c.r), srgb(c.g), srgb(c.b), c.a].iter().map(|&v| unorm(v, 8) as u8).collect(),
		T::Bgra8Unorm => [c.b, c.g, c.r, c.a].iter().map(|&v| unorm(v, 8) as u8).collect(),
		T::Bgra8UnormSrgb => [srgb(c.b), srgb(c.g), srgb(c.r), c.a].iter().map(|&v| unorm(v, 8) as u8).collect(),
		T::R8Unorm => vec![unorm(c.r, 8) as u8],
		T::Rg8Unorm => vec![unorm(c.r, 8) as u8, unorm(c.g, 8) as u8],
		T::R16Float => half(c.r).to_le_bytes()[..2].to_vec(),
		T::Rg16Float => [half(c.r), half(c.g)].iter().flat_map(|h| h.to_le_bytes()[..2].to_vec()).collect(),
		T::Rgba16Float => ch.iter().flat_map(|&v| half(v).to_le_bytes()[..2].to_vec()).collect(),
		T::R32Float => (c.r as f32).to_le_bytes().to_vec(),
		T::Rg32Float => [c.r as f32, c.g as f32].iter().flat_map(|v| v.to_le_bytes()).collect(),
		T::Rgba32Float => ch.iter().flat_map(|&v| (v as f32).to_le_bytes()).collect(),
		T::R32Uint | T::R32Sint => (c.r as u32).to_le_bytes().to_vec(),
		T::Rgb10a2Unorm => (unorm(c.r, 10) | unorm(c.g, 10) << 10 | unorm(c.b, 10) << 20 | unorm(c.a, 2) << 30).to_le_bytes().to_vec(),
		_ => {
			if ch.iter().all(|&v| v == 0.0) {
				vec![0; f.bytes as usize]
			} else {
				return None;
			}
		}
	};
	Some(bytes)
}

/// f32 to IEEE half bits (round to nearest, enough for clear colours).
fn half_bits(v: f32) -> u16 {
	let b = v.to_bits();
	let sign = ((b >> 16) & 0x8000) as u16;
	let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
	let man = b & 0x7f_ffff;
	if exp <= 0 {
		sign
	} else if exp >= 31 {
		sign | 0x7c00
	} else {
		sign | (exp as u16) << 10 | ((man + 0x1000) >> 13) as u16
	}
}

impl crate::CommandEncoder for CommandEncoder {
	type A = Api;

	unsafe fn begin_encoding(&mut self, label: crate::Label) -> DeviceResult<()> {
		self.words.clear();
		self.dirty = false;
		self.gfx = None;
		self.comp = None;
		self.skip = false;
		self.groups = Default::default();
		self.vbs = [None; 16];
		self.index = None;
		Ok(())
	}

	unsafe fn discard_encoding(&mut self) {
		self.words.clear();
		self.used.clear();
		for a in self.uploads.drain(..) {
			self.shared.low.free(a);
		}
		self.chunk = None;
	}

	unsafe fn end_encoding(&mut self) -> DeviceResult<CommandBuffer> {
		// Leave the GPU idle and the caches clean for whatever comes next.
		self.barrier();
		self.chunk = None;
		Ok(CommandBuffer { words: core::mem::take(&mut self.words), uploads: core::mem::take(&mut self.uploads), used: core::mem::take(&mut self.used), shared: Arc::clone(&self.shared) })
	}

	unsafe fn reset_all<I>(&mut self, command_buffers: I)
	where
		I: Iterator<Item = CommandBuffer>,
	{
		// Dropping them frees their uploads.
		for _ in command_buffers {}
	}

	unsafe fn transition_buffers<'a, T>(&mut self, barriers: T)
	where
		T: Iterator<Item = crate::BufferBarrier<'a, Buffer>>,
	{
		if barriers.count() > 0 {
			self.barrier_if_dirty();
		}
	}

	unsafe fn transition_textures<'a, T>(&mut self, barriers: T)
	where
		T: Iterator<Item = crate::TextureBarrier<'a, Texture>>,
	{
		if barriers.count() > 0 {
			self.barrier_if_dirty();
		}
	}

	unsafe fn clear_buffer(&mut self, buffer: &Buffer, range: crate::MemoryRange) {
		if range.end > range.start {
			let (a, n) = (buffer.addr() + range.start, range.end - range.start);
			if blit::mode() & 1 != 0 && n >= blit::BUFFER_FILL_MIN {
				self.blit_fill_buffer(a, n);
			} else {
				self.dma(None, 0, a, n);
			}
		}
	}

	unsafe fn copy_buffer_to_buffer<T>(&mut self, src: &Buffer, dst: &Buffer, regions: T)
	where
		T: Iterator<Item = crate::BufferCopy>,
	{
		for r in regions {
			self.dma(Some(src.addr() + r.src_offset), 0, dst.addr() + r.dst_offset, r.size.get());
		}
	}

	unsafe fn copy_texture_to_texture<T>(&mut self, src: &Texture, src_usage: wgt::TextureUses, dst: &Texture, regions: T)
	where
		T: Iterator<Item = crate::TextureCopy>,
	{
		if let Some(n) = self.shared.tracing() {
			std::eprintln!("GNM trace {n}: copy texture {:x} {:?} -> {:x}{} {:?}", src.addr, src.format, dst.addr, self.shared.screen_mark(dst.addr), dst.format);
		}
		if src.is_depth() || dst.is_depth() {
			self.shared.once("copy-depth", || String::from("copies of depth textures are not supported"));
			return;
		}
		let bpp = src.fmt.bytes as u64;
		for r in regions {
			for z in 0..r.size.depth {
				let (sl, dl) = (&src.levels[r.src_base.mip_level as usize], &dst.levels[r.dst_base.mip_level as usize]);
				let s0 = src.subresource(r.src_base.mip_level, r.src_base.array_layer + r.src_base.origin.z + z);
				let d0 = dst.subresource(r.dst_base.mip_level, r.dst_base.array_layer + r.dst_base.origin.z + z);
				for y in 0..r.size.height {
					let s = s0 + ((r.src_base.origin.y + y) as u64 * sl.pitch as u64 + r.src_base.origin.x as u64) * bpp;
					let d = d0 + ((r.dst_base.origin.y + y) as u64 * dl.pitch as u64 + r.dst_base.origin.x as u64) * bpp;
					self.dma(Some(s), 0, d, r.size.width as u64 * bpp);
				}
			}
		}
	}

	unsafe fn copy_buffer_to_texture<T>(&mut self, src: &Buffer, dst: &Texture, regions: T)
	where
		T: Iterator<Item = crate::BufferTextureCopy>,
	{
		if dst.probe {
			// wgputest's mip address probe: keep the offset fill (wgpu-core's zero init would come through here).
			return;
		}
		if dst.is_depth() {
			self.shared.once("upload-depth", || String::from("uploads into depth textures are not supported"));
			return;
		}
		let bpp = dst.fmt.bytes as u64;
		let trace = self.shared.tracing();
		for r in regions {
			let row = r.buffer_layout.bytes_per_row.map_or(r.size.width as u64 * bpp, |b| b as u64);
			// Frame trace: uploads into textures of 64 KB or more (the retail overlay is one), source and destination dumped once
			// the frame is done. `RAC_GNM_DUMP_UPLOADS=N`: the first N such uploads of the run, in any frame (the modern UI's glyph
			// atlas is written only when a page's text changes).
			// The whole destination level (what the GPU samples afterwards), so a small glyph upload still shows its atlas.
			let l = dst.levels[r.texture_base.mip_level as usize];
			let level_bytes = l.pitch as u64 * l.height as u64 * bpp;
			let n = if level_bytes >= 64 << 10 { { let f = self.shared.shown.load(core::sync::atomic::Ordering::Relaxed) + 1; trace.or_else(|| super::pipe::dump_upload(f).then_some(f)) } } else { None };
			if let Some(n) = n {
				let mut t = self.shared.trace_bufs.lock();
				// "row B" = bytes from one row to the next, so the raw file the dump writes can be turned into a picture.
				t.push((format!("frame {n} upload {}x{} at {},{} {:?} source row {row}", r.size.width, r.size.height, r.texture_base.origin.x, r.texture_base.origin.y, dst.format), src.addr() + r.buffer_layout.offset, row * r.size.height as u64));
				t.push((format!("frame {n} upload into {}x{} {:?} texture row {}", l.width, l.height, dst.format, l.pitch as u64 * bpp), dst.subresource(r.texture_base.mip_level, r.texture_base.array_layer), level_bytes));
			}
			let rows = r.buffer_layout.rows_per_image.map_or(r.size.height as u64, |v| v as u64);
			let l = dst.levels[r.texture_base.mip_level as usize];
			for z in 0..r.size.depth {
				let d0 = dst.subresource(r.texture_base.mip_level, r.texture_base.array_layer + r.texture_base.origin.z + z);
				let s0 = src.addr() + r.buffer_layout.offset + z as u64 * rows * row;
				let pitch_bytes = l.pitch as u64 * bpp;
				let whole = r.texture_base.origin.x == 0 && row == pitch_bytes && r.size.width == l.width;
				if whole {
					let d = d0 + r.texture_base.origin.y as u64 * pitch_bytes;
					self.dma(Some(s0), 0, d, row * r.size.height as u64);
					continue;
				}
				for y in 0..r.size.height {
					let d = d0 + (r.texture_base.origin.y + y) as u64 * pitch_bytes + r.texture_base.origin.x as u64 * bpp;
					self.dma(Some(s0 + y as u64 * row), 0, d, r.size.width as u64 * bpp);
				}
			}
		}
	}

	unsafe fn copy_texture_to_buffer<T>(&mut self, src: &Texture, src_usage: wgt::TextureUses, dst: &Buffer, regions: T)
	where
		T: Iterator<Item = crate::BufferTextureCopy>,
	{
		if src.is_depth() {
			self.shared.once("readback-depth", || String::from("readback of depth textures is not supported"));
			return;
		}
		let bpp = src.fmt.bytes as u64;
		for r in regions {
			let row = r.buffer_layout.bytes_per_row.map_or(r.size.width as u64 * bpp, |b| b as u64);
			let rows = r.buffer_layout.rows_per_image.map_or(r.size.height as u64, |v| v as u64);
			let l = src.levels[r.texture_base.mip_level as usize];
			for z in 0..r.size.depth {
				let s0 = src.subresource(r.texture_base.mip_level, r.texture_base.array_layer + r.texture_base.origin.z + z);
				let d0 = dst.addr() + r.buffer_layout.offset + z as u64 * rows * row;
				for y in 0..r.size.height {
					let s = s0 + (r.texture_base.origin.y + y) as u64 * l.pitch as u64 * bpp + r.texture_base.origin.x as u64 * bpp;
					self.dma(Some(s), 0, d0 + y as u64 * row, r.size.width as u64 * bpp);
				}
			}
		}
	}

	unsafe fn begin_query(&mut self, set: &Resource, index: u32) {}
	unsafe fn end_query(&mut self, set: &Resource, index: u32) {}
	unsafe fn write_timestamp(&mut self, set: &Resource, index: u32) {}
	unsafe fn read_acceleration_structure_compact_size(&mut self, acceleration_structure: &Resource, buf: &Buffer) {}
	unsafe fn reset_queries(&mut self, set: &Resource, range: core::ops::Range<u32>) {}
	unsafe fn copy_query_results(
		&mut self,
		set: &Resource,
		range: core::ops::Range<u32>,
		buffer: &Buffer,
		offset: wgt::BufferAddress,
		stride: wgt::BufferSize,
	) {
	}

	// Render passes.

	unsafe fn begin_render_pass(&mut self, desc: &crate::RenderPassDescriptor<Resource, TextureView>) -> DeviceResult<()> {
		self.barrier_if_dirty();
		self.prof_begin(desc.label);
		if blit::mode() != 0 {
			self.blit_pass = format!("{:?}", desc.label);
		}
		if let Some(n) = self.shared.tracing() {
			let mut t = format!("GNM trace {n}: pass {:?}:", desc.label);
			for ca in desc.color_attachments.iter().flatten() {
				let v = ca.target.view;
				t += &format!(
					" colour {:x} {}x{} {:?} {:?}{}",
					v.addr,
					v.level.width,
					v.level.height,
					v.format,
					ca.ops,
					self.shared.screen_mark(v.addr)
				);
				if let Some(r) = &ca.resolve_target {
					t += &format!(" -> resolve {:x}{}", r.view.addr, self.shared.screen_mark(r.view.addr));
				}
			}
			if let Some(ds) = &desc.depth_stencil_attachment {
				t += &format!(" depth {:x} {:?} {:?}", ds.target.view.addr, ds.target.view.format, ds.depth_ops);
			}
			std::eprintln!("{t}");
			self.trace_draws = 0;
			self.trace_pass = format!("{:?}", desc.label);
			self.trace_dumped = false;
			self.trace_target = desc.color_attachments.iter().flatten().next().map(|ca| {
				let v = ca.target.view;
				(v.addr, v.level.pitch, v.level.width, v.level.height, v.fmt.bytes, v.format, self.shared.screen_mark(v.addr))
			});
		}
		// Clears first (CP DMA), then a barrier so the colour / depth units see them. With `RAC_GNM_BLIT=clear` (blit.rs) the
		// targets that can take it are cleared by a draw instead, after that barrier.
		let blit_clears = blit::mode() & 1 != 0;
		let mut draw_colour: Vec<(TextureView, wgt::Color)> = Vec::new();
		// (view, depth to write, stencil value to write): the depth / stencil clears that take one draw.
		let mut draw_depth: Option<(TextureView, Option<f32>, Option<u8>)> = None;
		let mut cleared = false;
		for ca in desc.color_attachments.iter().flatten() {
			if ca.ops.contains(crate::AttachmentOps::LOAD_CLEAR) {
				let v = ca.target.view;
				if blit_clears && blit::colour_ok(v) {
					draw_colour.push((*v, ca.clear_value));
					continue;
				}
				let bytes = v.level.layer_bytes * v.layers.max(1) as u64;
				match (clear_pattern(&v.fmt, v.format, ca.clear_value), clear_texel(&v.fmt, v.format, ca.clear_value)) {
					(Some(pat), _) => self.dma(None, pat, v.addr, bytes),
					// 8 / 16-byte texels without a 4-byte period: a block of repeated texels in the low heap, copied
					// over the whole range (rows are contiguous, padding included).
					(None, Some(texel)) => self.fill_texels(&texel, v.addr, bytes),
					(None, None) => {
						self.shared.once("clear-colour", || format!("clear colour {:?} of {:?} is not supported: cleared to zero", ca.clear_value, v.format));
						self.dma(None, 0, v.addr, bytes);
					}
				}
				cleared = true;
			}
		}
		if let Some(ds) = &desc.depth_stencil_attachment {
			let v = ds.target.view;
			if ds.depth_ops.contains(crate::AttachmentOps::LOAD_CLEAR) && v.fmt.depth.is_some_and(|(z, _)| z != 0) && blit_clears && blit::depth_ok(v) {
				draw_depth = Some((*v, Some(ds.clear_value.0.clamp(0.0, 1.0)), None));
			} else if ds.depth_ops.contains(crate::AttachmentOps::LOAD_CLEAR) && v.fmt.depth.is_some_and(|(z, _)| z != 0) {
				let value = if v.fmt.depth == Some((1, false)) {
					let z = (ds.clear_value.0.clamp(0.0, 1.0) * 65535.0).round() as u32;
					z | z << 16
				} else {
					ds.clear_value.0.to_bits()
				};
				self.dma(None, value, v.addr, v.level.layer_bytes * v.layers.max(1) as u64);
				cleared = true;
			}
			if ds.stencil_ops.contains(crate::AttachmentOps::LOAD_CLEAR) && v.fmt.depth.is_some_and(|(_, s)| s) && blit_clears && blit::depth_ok(v) {
				// The stencil plane in the same draw (stencil test Always, Replace with the clear value).
				let s = (ds.clear_value.1 & 0xff) as u8;
				match &mut draw_depth {
					Some(d) => d.2 = Some(s),
					None => draw_depth = Some((*v, None, Some(s))),
				}
			} else if ds.stencil_ops.contains(crate::AttachmentOps::LOAD_CLEAR) && v.fmt.depth.is_some_and(|(_, s)| s) {
				let s = ds.clear_value.1 & 0xff;
				self.dma(None, s * 0x0101_0101, v.stencil_addr, v.level.stencil_layer_bytes * v.layers.max(1) as u64);
				cleared = true;
			}
		}
		if cleared {
			self.barrier();
		}
		if !draw_colour.is_empty() || draw_depth.is_some() {
			self.blit_clears(&draw_colour, draw_depth, desc.extent.width, desc.extent.height);
		}
		for i in 0..8u32 {
			let v = desc.color_attachments.get(i as usize).and_then(|c| c.as_ref()).map(|c| c.target.view);
			self.color_target(i, v);
		}
		self.resolves.clear();
		for ca in desc.color_attachments.iter().flatten() {
			if let Some(r) = &ca.resolve_target {
				let (s, d) = (ca.target.view, r.view);
				self.resolves.push(Resolve {
					src: (s.addr, s.level.pitch, s.level.layer_bytes, s.layers.max(1)),
					dst: (d.addr, d.level.pitch, d.level.layer_bytes, d.layers.max(1)),
					bytes: s.fmt.bytes,
					width: s.level.width.min(d.level.width),
					height: s.level.height.min(d.level.height),
					blit: (blit::mode() & 2 != 0 && blit::colour_ok(s) && blit::colour_ok(d) && s.format == d.format).then(|| (*d, s.image)),
				});
			}
		}
		self.depth_target(desc.depth_stencil_attachment.as_ref().map(|d| d.target.view));
		let (w, h) = (desc.extent.width, desc.extent.height);
		let br = h << 16 | w;
		{
			let mut wr = self.w();
			wr.ctx(regs::PA_SC_SCREEN_SCISSOR_TL, &[0, br]);
			wr.ctx(regs::PA_SC_WINDOW_OFFSET, &[0, 1 << 31, br]);
			wr.ctx(regs::PA_SC_AA_CONFIG, &[0]);
			wr.ctx(regs::PA_SC_AA_MASK_X0Y0_X1Y0, &[0xffff_ffff, 0xffff_ffff]);
		}
		self.viewport(&crate::Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 }, 0.0..1.0);
		self.scissor(&crate::Rect { x: 0, y: 0, w, h });
		self.gfx = None;
		self.skip = false;
		self.dirty = true;
		Ok(())
	}

	unsafe fn end_render_pass(&mut self) {
		self.dirty = true;
		self.prof_end();
		if let Some(n) = self.shared.tracing() {
			std::eprintln!("GNM trace {n}:   {} draws", self.trace_draws);
			// The middle row of the first colour target once the pass's draws have landed (printed at present).
			if let Some((addr, pitch, width, height, tb, format, _)) = self.trace_target.take() {
				if let Some(dst) = self.shared.snap_slot(format!("frame {n} after pass {}", self.trace_pass), width, tb, format) {
					self.barrier();
					self.dma(Some(addr + (height / 2) as u64 * pitch as u64 * tb as u64), 0, dst, width as u64 * tb as u64);
				}
			}
		}
		if self.resolves.is_empty() {
			return;
		}
		// The draws must land before the CP copies the target (or a copy draw reads it through the texture cache).
		self.barrier();
		let mut unbound = false;
		for r in core::mem::take(&mut self.resolves) {
			if let Some((dv, image)) = r.blit {
				if !unbound {
					for i in 1..8 {
						self.color_target(i, None);
					}
					self.depth_target(None);
					unbound = true;
				}
				self.color_target(0, Some(&dv));
				let ps = blit::copy_regs(self.shared.blit_code() + blit::COPY_AT as u64);
				self.blit_draw(&ps, 0.0, &image, None, r.width, r.height);
				if self.blit_checking() {
					// The source's middle row against the destination's, after the copy has landed.
					self.barrier();
					let (row, y) = (r.width as u64 * r.bytes as u64, r.height as u64 / 2);
					let (s, d) = (r.src.0 + y * r.src.1 as u64 * r.bytes as u64, r.dst.0 + y * r.dst.1 as u64 * r.bytes as u64);
					if let (Some(a), Some(b)) = (self.shared.snap_raw(row as usize), self.shared.snap_raw(row as usize)) {
						self.dma(Some(d), 0, a, row);
						self.dma(Some(s), 0, b, row);
						let what = format!("resolve {}x{} {:?} in pass {}", r.width, r.height, dv.format, self.blit_pass);
						self.shared.blit_checks.lock().push(blit::Check { what, got: a, want: blit::Want::Row(b), bytes: row as usize });
					}
				}
				continue;
			}
			let ((sa, sp, sl, sn), (da, dp, dl, dn)) = (r.src, r.dst);
			let row = r.width as u64 * r.bytes as u64;
			for layer in 0..sn.min(dn) as u64 {
				let (s0, d0) = (sa + sl * layer, da + dl * layer);
				if sp == dp {
					// Same pitch: the rows are one contiguous run.
					self.dma(Some(s0), 0, d0, sp as u64 * r.bytes as u64 * (r.height as u64 - 1) + row);
				} else {
					for y in 0..r.height as u64 {
						self.dma(Some(s0 + y * sp as u64 * r.bytes as u64), 0, d0 + y * dp as u64 * r.bytes as u64, row);
					}
				}
			}
		}
	}

	unsafe fn set_bind_group(&mut self, layout: &PipelineLayout, index: u32, group: &BindGroup, dynamic_offsets: &[wgt::DynamicOffset]) {
		let dynamic: Vec<(u64, u32)> = group
			.dynamic
			.iter()
			.enumerate()
			.map(|(i, &(addr, size))| (addr + dynamic_offsets.get(i).copied().unwrap_or(0) as u64, size))
			.collect();
		let addr = group.mem.map_or(0, |a| a.addr() as u32);
		// Frame trace: the first group 0 bound in each pass, its buffers dumped at present (Queue::present).
		if index == 0 && !self.trace_dumped {
			if let Some(n) = self.shared.tracing() {
				self.trace_dumped = true;
				let mut t = self.shared.trace_bufs.lock();
				for &(b, a, size) in &group.bufs {
					t.push((format!("frame {n} pass {} group 0 binding {b}", self.trace_pass), a, size));
				}
				for (i, &(a, size)) in dynamic.iter().enumerate() {
					t.push((format!("frame {n} pass {} group 0 dynamic {i}", self.trace_pass), a, size as u64));
				}
			}
		}
		// Frame trace, overlay pipelines bound: the descriptors of each texture group (T# then S#), to check the overlay's image.
		if index == 1 || self.trace_pass.contains("modern ui") {
			let hud = self.trace_pass.contains("modern ui") || self.gfx.as_ref().is_some_and(|g| super::pipe::overlay_like(&g.label, &g.entries));
			if let (Some(n), Some(m)) = (self.shared.tracing().filter(|_| hud), group.mem.as_ref()) {
				// SAFETY: a live bind group's descriptor memory (`len` bytes).
				let w = unsafe { core::slice::from_raw_parts(m.ptr as *const u32, (m.len / 4).min(24)) };
				let hex: Vec<String> = w.iter().map(|x| format!("{x:08x}")).collect();
				std::eprintln!("GNM trace {n}:   group {index} at {addr:x}: {}", hex.join(" "));
			}
		}
		if let Some(g) = self.groups.get_mut(index as usize) {
			*g = Some(Bound { addr, dynamic });
		}
	}

	unsafe fn set_immediates(&mut self, layout: &PipelineLayout, offset_bytes: u32, data: &[u32]) {
		let at = offset_bytes as usize / 4;
		for (i, &v) in data.iter().enumerate() {
			if let Some(d) = self.immediates.get_mut(at + i) {
				*d = v;
			}
		}
	}

	unsafe fn insert_debug_marker(&mut self, label: &str) {}
	unsafe fn begin_debug_marker(&mut self, group_label: &str) {}
	unsafe fn end_debug_marker(&mut self) {}

	unsafe fn set_render_pipeline(&mut self, pipeline: &RenderPipeline) {
		self.gfx = pipeline.inner.clone();
		self.skip = self.gfx.is_none();
		// RAC_GNM_AB_SKIP (pipe::ab_skipped): this pipeline draws nothing in the current frame window.
		if let Some(g) = &self.gfx {
			if !self.used.contains(&g.key) {
				self.used.push(g.key);
			}
			if super::pipe::ab_skipped(self.shared.shown.load(core::sync::atomic::Ordering::Relaxed) + 1, g.key, &g.label) {
				self.skip = true;
			}
		}
		if let Some(g) = self.gfx.clone() {
			// Frame trace: which pipeline each group of overlay quads takes (every pipeline bound with `RAC_GNM_TRACE_PIPES=1`).
			if let Some(n) = self.shared.tracing().filter(|_| super::pipe::trace_all_pipes() || super::pipe::overlay_like(&g.label, &g.entries)) {
				std::eprintln!("GNM trace {n}:   pipeline {:016x} {} {} (after {} draws)", g.key, g.label, g.entries, self.trace_draws);
			}
			self.words.extend_from_slice(&g.pm4);
			// RAC_GNM_AB (Shared::ab_mode): override the pipeline's blending / depth state for a console A/B picture.
			let ab = self.shared.ab_mode();
			if ab & 1 != 0 {
				self.w().ctx(regs::CB_BLEND0_CONTROL, &[0; 8]);
			}
			if ab & 2 != 0 {
				self.w().ctx(regs::DB_DEPTH_CONTROL, &[0]);
			}
			self.stencil_refmask();
		}
	}

	unsafe fn set_index_buffer<'a>(&mut self, binding: crate::BufferBinding<'a, Buffer>, format: wgt::IndexFormat) {
		let size = binding.size.map_or(binding.buffer.size.saturating_sub(binding.offset), |s| s.get());
		self.index = Some((binding.buffer.addr() + binding.offset, size, format));
	}

	unsafe fn set_vertex_buffer<'a>(&mut self, index: u32, binding: crate::BufferBinding<'a, Buffer>) {
		let size = binding.size.map_or(binding.buffer.size.saturating_sub(binding.offset), |s| s.get());
		if let Some(v) = self.vbs.get_mut(index as usize) {
			*v = Some((binding.buffer.addr() + binding.offset, size));
		}
	}

	unsafe fn set_viewport(&mut self, rect: &crate::Rect<f32>, depth_range: core::ops::Range<f32>) {
		self.viewport(rect, depth_range);
	}

	unsafe fn set_scissor_rect(&mut self, rect: &crate::Rect<u32>) {
		self.scissor(rect);
	}

	unsafe fn set_stencil_reference(&mut self, value: u32) {
		self.stencil_ref = value;
		self.stencil_refmask();
	}

	unsafe fn set_blend_constants(&mut self, color: &[f32; 4]) {
		self.w().ctx(CB_BLEND_RED, &[color[0].to_bits(), color[1].to_bits(), color[2].to_bits(), color[3].to_bits()]);
	}

	unsafe fn draw(&mut self, first_vertex: u32, vertex_count: u32, first_instance: u32, instance_count: u32) {
		if self.skip || self.gfx.is_none() || vertex_count == 0 || instance_count == 0 {
			if self.skip {
				self.shared.skipped.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
			}
			return;
		}
		self.trace_draws += 1;
		self.graphics_user_data(first_vertex as i32, first_instance);
		self.w().pkt3(op::NUM_INSTANCES, &[instance_count]);
		plat::draw_auto(self.w().reserve(7), vertex_count);
		self.dirty = true;
	}

	unsafe fn draw_indexed(&mut self, first_index: u32, index_count: u32, base_vertex: i32, first_instance: u32, instance_count: u32) {
		if self.skip || self.gfx.is_none() || index_count == 0 || instance_count == 0 {
			if self.skip {
				self.shared.skipped.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
			}
			return;
		}
		let Some((addr, size, format)) = self.index else { return };
		let isize = if format == wgt::IndexFormat::Uint16 { 2 } else { 4 };
		self.trace_draws += 1;
		self.graphics_user_data(base_vertex, first_instance);
		let base = addr + first_index as u64 * isize;
		let max = (size / isize).saturating_sub(first_index as u64) as u32;
		let mut w = self.w();
		w.pkt3(op::INDEX_TYPE, &[(isize == 4) as u32]);
		w.pkt3(op::NUM_INSTANCES, &[instance_count]);
		// DRAW_INDEX_2: max size, index base, count, DI_SRC_SEL_DMA.
		w.pkt3(OP_DRAW_INDEX_2, &[max, base as u32, (base >> 32) as u32, index_count, 0]);
		self.dirty = true;
	}

	unsafe fn draw_mesh_tasks(&mut self, group_count_x: u32, group_count_y: u32, group_count_z: u32) {}

	unsafe fn draw_indirect(&mut self, buffer: &Buffer, offset: wgt::BufferAddress, draw_count: u32) {
		self.indirect(buffer, offset, draw_count, false);
	}

	unsafe fn draw_indexed_indirect(&mut self, buffer: &Buffer, offset: wgt::BufferAddress, draw_count: u32) {
		self.indirect(buffer, offset, draw_count, true);
	}

	unsafe fn draw_mesh_tasks_indirect(&mut self, buffer: &Buffer, offset: wgt::BufferAddress, draw_count: u32) {}

	unsafe fn draw_indirect_count(&mut self, buffer: &Buffer, offset: wgt::BufferAddress, count_buffer: &Buffer, count_offset: wgt::BufferAddress, max_count: u32) {
		self.shared.once("indirect", || String::from("indirect draws are not supported (skipped)"));
	}

	unsafe fn draw_indexed_indirect_count(
		&mut self,
		buffer: &Buffer,
		offset: wgt::BufferAddress,
		count_buffer: &Buffer,
		count_offset: wgt::BufferAddress,
		max_count: u32,
	) {
		self.shared.once("indirect", || String::from("indirect draws are not supported (skipped)"));
	}

	unsafe fn draw_mesh_tasks_indirect_count(
		&mut self,
		buffer: &Buffer,
		offset: wgt::BufferAddress,
		count_buffer: &Buffer,
		count_offset: wgt::BufferAddress,
		max_count: u32,
	) {
	}

	// Compute.

	unsafe fn begin_compute_pass(&mut self, desc: &crate::ComputePassDescriptor<Resource>) {
		if let Some(n) = self.shared.tracing() {
			std::eprintln!("GNM trace {n}: compute pass {:?}", desc.label);
		}
		self.barrier_if_dirty();
		self.prof_begin(desc.label);
		self.comp = None;
		self.skip = false;
	}

	unsafe fn end_compute_pass(&mut self) {
		self.dirty = true;
		self.prof_end();
	}

	unsafe fn set_compute_pipeline(&mut self, pipeline: &ComputePipeline) {
		self.comp = pipeline.inner.clone();
		self.skip = self.comp.is_none();
		if let Some(c) = self.comp.clone() {
			if !self.used.contains(&c.key) {
				self.used.push(c.key);
			}
			self.words.extend_from_slice(&c.pm4);
		}
	}

	unsafe fn dispatch_workgroups(&mut self, count: [u32; 3]) {
		if self.skip || self.comp.is_none() || count.contains(&0) {
			if self.skip {
				self.shared.skipped.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
			}
			return;
		}
		// radv before GFX10.3 passes the grid size as a 32-bit pointer to three dwords (one SGPR), not as values.
		let grid = match self.comp.as_ref().and_then(|c| c.cs.ud(ud::CS_GRID_SIZE)) {
			Some(l) if l.count == 1 => vec![self.upload(&count) as u32],
			_ => count.to_vec(),
		};
		self.user_data(2, COMPUTE_USER_DATA_0, &[(ud::CS_GRID_SIZE, &grid[..])]);
		// DISPATCH_DIRECT with the compute shader type bit; COMPUTE_SHADER_EN | FORCE_START_AT_000.
		self.words.push(pm4::header(OP_DISPATCH_DIRECT, 4) | 1 << 1);
		self.words.extend_from_slice(&[count[0], count[1], count[2], 1 | 1 << 2]);
		self.dirty = true;
		// RAC_GNM_CS_TRACE=1: drain after the dispatch and write its id to the dispatch label (the hang report names it).
		if let Some(label) = self.shared.cs_trace() {
			let key = self.comp.as_ref().map_or(0, |c| c.key);
			let params = self.groups[0].as_ref().and_then(|b| b.dynamic.first()).map_or(0, |d| d.0);
			let id = self.shared.cs_note(self as *const Self as usize, key, count, params);
			let mut w = self.w();
			// CS_PARTIAL_FLUSH, then an end-of-pipe write of the id (CACHE_FLUSH_AND_INV_TS_EVENT, 32-bit data) and a wait for it.
			w.pkt3(OP_EVENT_WRITE, &[7 | 4 << 8]);
			w.pkt3(op::EVENT_WRITE_EOP, &[0x14 | 5 << 8, label as u32, ((label >> 32) as u32 & 0xffff) | 1 << 29, id, 0]);
			w.pkt3(OP_WAIT_REG_MEM, &[3 | 1 << 4, label as u32, (label >> 32) as u32, id, 0xffff_ffff, 10]);
		}
	}

	unsafe fn dispatch_workgroups_indirect(&mut self, buffer: &Buffer, offset: wgt::BufferAddress) {
		if self.skip || self.comp.is_none() {
			if self.skip {
				self.shared.skipped.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
			}
			return;
		}
		let args = buffer.addr() + offset;
		// The grid size SGPR is a 32-bit pointer on GFX7 and the buffer may sit above 4 GB, so a CP DMA copies the three
		// dwords into the low heap first (in order: the DMA waits before the dispatch is read).
		let mut low = None;
		if self.comp.as_ref().and_then(|c| c.cs.ud(ud::CS_GRID_SIZE)).is_some() {
			let a = self.upload(&[0; 4]);
			self.dma(Some(args), 0, a, 12);
			low = Some([a as u32]);
		}
		match &low {
			Some(v) => self.user_data(2, COMPUTE_USER_DATA_0, &[(ud::CS_GRID_SIZE, v)]),
			None => self.user_data(2, COMPUTE_USER_DATA_0, &[]),
		}
		let mut w = self.w();
		// SET_BASE 1 (the indirect argument base), then DISPATCH_INDIRECT at offset 0 with the same initiator as direct.
		w.0.push(pm4::header(OP_SET_BASE, 3) | 1 << 1);
		w.0.extend_from_slice(&[1, args as u32, (args >> 32) as u32]);
		w.0.push(pm4::header(OP_DISPATCH_INDIRECT, 2) | 1 << 1);
		w.0.extend_from_slice(&[0, 1 | 1 << 2]);
		self.dirty = true;
	}

	unsafe fn build_acceleration_structures<'a, T>(&mut self, _descriptor_count: u32, descriptors: T)
	where
		Api: 'a,
		T: IntoIterator<Item = crate::BuildAccelerationStructureDescriptor<'a, Buffer, Resource>>,
	{
	}

	unsafe fn place_acceleration_structure_barrier(&mut self, _barriers: crate::AccelerationStructureBarrier) {}

	unsafe fn copy_acceleration_structure_to_acceleration_structure(&mut self, src: &Resource, dst: &Resource, copy: wgt::AccelerationStructureCopy) {}

	unsafe fn set_acceleration_structure_dependencies(command_buffers: &[&CommandBuffer], dependencies: &[&Resource]) {}

	unsafe fn begin_ray_tracing_pass(&mut self, _desc: &crate::RayTracingPassDescriptor) {}

	unsafe fn end_ray_tracing_pass(&mut self) {}

	unsafe fn set_ray_tracing_pipeline(&mut self, _pipeline: &Resource) {}

	unsafe fn trace_rays(
		&mut self,
		_count: [u32; 3],
		_ray_generation_group_data: crate::PipelineGroupData<Buffer>,
		_miss_group_data: crate::PipelineGroupData<Buffer>,
		_intersection_group_data: crate::PipelineGroupData<Buffer>,
	) {
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn half_and_patterns() {
		assert_eq!(half_bits(1.0), 0x3c00);
		assert_eq!(half_bits(0.0), 0);
		assert_eq!(half_bits(-2.0), 0xc000);
		let f = super::super::format::of(wgt::TextureFormat::Rgba16Float).unwrap();
		assert_eq!(clear_pattern(&f, wgt::TextureFormat::Rgba16Float, wgt::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }), Some(0));
		assert_eq!(clear_pattern(&f, wgt::TextureFormat::Rgba16Float, wgt::Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }), Some(0x3c00_3c00));
		assert_eq!(clear_pattern(&f, wgt::TextureFormat::Rgba16Float, wgt::Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }), None);
		// The game's (0, 0, 0, 1) clear: no 4-byte period, so it goes through `fill_texels` with these 8 bytes.
		assert_eq!(clear_texel(&f, wgt::TextureFormat::Rgba16Float, wgt::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }), Some(vec![0, 0, 0, 0, 0, 0, 0, 0x3c]));
		let b = super::super::format::of(wgt::TextureFormat::Bgra8Unorm).unwrap();
		assert_eq!(clear_pattern(&b, wgt::TextureFormat::Bgra8Unorm, wgt::Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }), Some(0xffff_0000));
	}
}
