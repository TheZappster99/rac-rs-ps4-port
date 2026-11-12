//! PS4 port, step 4 test: a small wgpu program drawn through the GNM back end (see Cargo.toml for the three ways to run it).
//!
//! Each frame: a background from immediates (push constants), then a textured, vertex-coloured, spinning cube with depth,
//! drawn into an offscreen Bgra8UnormSrgb target (the PS4 display format, so the pipelines are the ones the display would
//! use); the target is copied to the surface (PS4 only) and, on a few frames, read back so the log shows real pixels.
//! Log: /data/rac-rs/wgputest.log on the PS4, stderr on the PC.

use std::io::Write;
use std::sync::Mutex;

const W: u32 = 1920;
const H: u32 = 1080;
const FRAMES: u32 = 600;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;

const SHADER: &str = r#"
struct U { mvp: mat4x4<f32>, tint: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;

struct VO { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) col: vec3<f32> };

@vertex fn vs_cube(@location(0) p: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) c: vec3<f32>) -> VO {
	var o: VO;
	o.pos = u.mvp * vec4<f32>(p, 1.0);
	o.uv = uv;
	o.col = c;
	return o;
}

@fragment fn fs_cube(v: VO) -> @location(0) vec4<f32> {
	return vec4<f32>(textureSample(tex, smp, v.uv).rgb * v.col * u.tint.rgb, 1.0);
}

var<immediate> bg: vec4<f32>;

struct BO { @builtin(position) pos: vec4<f32>, @location(0) t: f32 };

@vertex fn vs_bg(@builtin(vertex_index) i: u32) -> BO {
	let x = f32(i32(i & 1u) * 4 - 1);
	let y = f32(i32(i >> 1u) * 4 - 1);
	var o: BO;
	o.pos = vec4<f32>(x, y, 0.0, 1.0);
	o.t = y * 0.5 + 0.5;
	return o;
}

@fragment fn fs_bg(v: BO) -> @location(0) vec4<f32> {
	return vec4<f32>(bg.rgb * (0.4 + 0.6 * v.t), 1.0);
}
"#;

/// The mip / indirect check (`checks`): row r of a 16x3 target is drawn by instance r; pixel x samples level x (clamped
/// by the view) of texture A (rows 0 and 2) or B (row 1).
const MIP_SHADER: &str = r#"
@group(0) @binding(0) var ma: texture_2d<f32>;
@group(0) @binding(1) var mb: texture_2d<f32>;
@group(0) @binding(2) var ms: sampler;

struct MO { @builtin(position) pos: vec4<f32>, @location(0) @interpolate(flat) row: u32 };

@vertex fn vs_mip(@builtin(vertex_index) i: u32, @builtin(instance_index) r: u32) -> MO {
	var c = array<vec2<f32>, 6>(vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0));
	let p = c[i];
	let y0 = 1.0 - 2.0 * f32(r) / 3.0;
	var o: MO;
	o.pos = vec4<f32>(p.x * 2.0 - 1.0, y0 - p.y * 2.0 / 3.0, 0.0, 1.0);
	o.row = r;
	return o;
}

@fragment fn fs_mip(m: MO) -> @location(0) vec4<f32> {
	let lv = floor(m.pos.x);
	if (m.row == 1u) {
		return textureSampleLevel(mb, ms, vec2<f32>(0.5, 0.5), lv);
	}
	return textureSampleLevel(ma, ms, vec2<f32>(0.5, 0.5), lv);
}
"#;

/// The mip address probe (`probe`): for texture k and level l, the words at texels (0,0), (1,0), (0,1), (1,1) (all ones
/// where the level is too small) and the level's size.
const PROBE_SHADER: &str = r#"
@group(0) @binding(0) var t0: texture_2d<u32>;
@group(0) @binding(1) var t1: texture_2d<u32>;
@group(0) @binding(2) var t2: texture_2d<u32>;
@group(0) @binding(3) var t3: texture_2d<u32>;
@group(0) @binding(4) var t4: texture_2d<u32>;
@group(0) @binding(5) var<storage, read_write> out: array<u32>;

fn one(t: texture_2d<u32>, k: u32) {
	let n = textureNumLevels(t);
	for (var l = 0u; l < n; l++) {
		let d = textureDimensions(t, l);
		let at = (k * 16u + l) * 4u;
		out[at] = textureLoad(t, vec2<i32>(0, 0), i32(l)).x;
		out[at + 1u] = select(0xffffffffu, textureLoad(t, vec2<i32>(1, 0), i32(l)).x, d.x > 1u);
		out[at + 2u] = select(0xffffffffu, textureLoad(t, vec2<i32>(0, 1), i32(l)).x, d.y > 1u);
		out[at + 3u] = select(0xffffffffu, textureLoad(t, vec2<i32>(1, 1), i32(l)).x, d.x > 1u && d.y > 1u);
		out[320u + k * 16u + l] = d.x | (d.y << 16u);
	}
}

@compute @workgroup_size(1) fn probe() {
	one(t0, 0u);
	one(t1, 1u);
	one(t2, 2u);
	one(t3, 3u);
	one(t4, 4u);
}
"#;

/// Where the texture unit reads each mip level from (decides the hardware's linear mip layout). On the GNM back end with
/// `RAC_GNM_FILL_OFFSETS=1` each probe texture's memory holds its own word indices, so a load returns its address. Logs,
/// per level, the base, the step between texels and the row pitch the hardware used, next to rule a's prediction. On
/// Vulkan the numbers mean nothing (it only makes the capture).
fn probe(device: &wgpu::Device, queue: &wgpu::Queue) {
	let specs: [(wgpu::TextureFormat, u32, u32, u32, u32); 5] = [
		(wgpu::TextureFormat::R32Uint, 4, 256, 256, 9),
		(wgpu::TextureFormat::R32Uint, 4, 64, 48, 7),
		(wgpu::TextureFormat::R32Uint, 4, 16, 16, 5),
		(wgpu::TextureFormat::Rg32Uint, 8, 64, 64, 7),
		(wgpu::TextureFormat::Rgba32Uint, 16, 32, 32, 6),
	];
	// SAFETY: single-threaded here.
	unsafe { std::env::set_var("RAC_GNM_FILL_OFFSETS", "1") };
	let texs: Vec<wgpu::Texture> = specs
		.iter()
		.map(|&(format, _, w, h, levels)| {
			device.create_texture(&wgpu::TextureDescriptor {
				label: Some("mip probe"),
				size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
				mip_level_count: levels,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format,
				// COPY_DST: wgpu-core then zero-inits through a buffer copy, which the back end drops for probe textures.
				// Without it these renderable formats are cleared with a render pass, which wiped the fill in run 5.
				usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
				view_formats: &[],
			})
		})
		.collect();
	unsafe { std::env::remove_var("RAC_GNM_FILL_OFFSETS") };
	let views: Vec<wgpu::TextureView> = texs.iter().map(|t| t.create_view(&Default::default())).collect();
	let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("mip probe"), source: wgpu::ShaderSource::Wgsl(PROBE_SHADER.into()) });
	let mut entries: Vec<wgpu::BindGroupLayoutEntry> = (0..5)
		.map(|b| wgpu::BindGroupLayoutEntry {
			binding: b,
			visibility: wgpu::ShaderStages::COMPUTE,
			ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Uint, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
			count: None,
		})
		.collect();
	entries.push(wgpu::BindGroupLayoutEntry { binding: 5, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None }, count: None });
	let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("mip probe"), entries: &entries });
	let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("mip probe"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
	let pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("mip probe"), layout: Some(&layout), module: &module, entry_point: Some("probe"), compilation_options: Default::default(), cache: None });
	let size = 400 * 4;
	let out = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mip probe out"), size, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
	let rb = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mip probe readback"), size, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
	let mut be: Vec<wgpu::BindGroupEntry> = views.iter().enumerate().map(|(i, v)| wgpu::BindGroupEntry { binding: i as u32, resource: wgpu::BindingResource::TextureView(v) }).collect();
	be.push(wgpu::BindGroupEntry { binding: 5, resource: out.as_entire_binding() });
	let bind = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("mip probe"), layout: &bgl, entries: &be });
	let mut enc = device.create_command_encoder(&Default::default());
	{
		let mut pass = enc.begin_compute_pass(&Default::default());
		pass.set_pipeline(&pipe);
		pass.set_bind_group(0, &bind, &[]);
		pass.dispatch_workgroups(1, 1, 1);
	}
	enc.copy_buffer_to_buffer(&out, 0, &rb, 0, size);
	queue.submit([enc.finish()]);
	rb.slice(..).map_async(wgpu::MapMode::Read, |_| {});
	let _ = device.poll(wgpu::PollType::wait_indefinitely());
	let Ok(data) = rb.slice(..).get_mapped_range() else {
		say("wgputest: probe: readback not mapped");
		return;
	};
	let word = |i: usize| u32::from_le_bytes([data[i * 4], data[i * 4 + 1], data[i * 4 + 2], data[i * 4 + 3]]);
	for (k, &(format, bpe, w, h, levels)) in specs.iter().enumerate() {
		say(&format!("wgputest: probe {k}: {format:?} {w}x{h}, {levels} levels (bytes: hardware | back end)"));
		// The back end's rule (`format::mip_pitch`): pitch max(base >> l, max(8, 64 / bpe), 64 / rows), rows to a power
		// of two, levels on 256 B.
		let align = (64 / bpe).max(8);
		let base_pitch = w.next_power_of_two().max(align);
		let mut off = 0u64;
		for l in 0..levels {
			let at = (k * 16 + l as usize) * 4;
			let [a00, a10, a01, a11] = [word(at), word(at + 1), word(at + 2), word(at + 3)];
			let dims = word(320 + k * 16 + l as usize);
			let b = |v: u32| if v == u32::MAX { String::from("-") } else { format!("{}", v as i64 * 4) };
			let d = |v: u32| if v == u32::MAX { String::from("-") } else { format!("{}", (v as i64 - a00 as i64) * 4) };
			let rows = (h >> l).max(1).next_power_of_two();
			let pitch = (base_pitch >> l).max(1).next_power_of_two().max(align).max(64 / rows);
			say(&format!(
				"wgputest: probe {k} L{l} {}x{}: base {} | {off}, x step {} | {bpe}, row step {} | {}, (1,1) {}",
				dims & 0xffff,
				dims >> 16,
				b(a00),
				d(a10),
				d(a01),
				pitch * bpe,
				d(a11)
			));
			off = (off + (pitch * rows * bpe) as u64).next_multiple_of(256);
		}
	}
	drop(data);
	rb.unmap();
}

/// Level `i`'s solid colour in the mip check (R G B A bytes).
fn level_colour(i: u32) -> [u8; 4] {
	[(10 + 25 * i) as u8, (250 - 25 * i) as u8, (30 * i) as u8, 255]
}

/// Mipmapped textures, indirect draws and an 8-byte clear: draws the mip check (A: 256x256, 9 levels; B: 64x48, 7
/// levels, so the power-of-two padding of heights is tested too) with one multi-draw-indirect (rows 0 and 1, first
/// instance 0 and 1) and one indexed indirect draw (row 2), clears an Rgba16Float target to (0, 0, 0, 1), reads both back
/// and logs PASS / FAIL per row. `tag` names the run (the GNM back end's mip rule).
fn checks(device: &wgpu::Device, queue: &wgpu::Queue, tag: &str) -> bool {
	let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("mipcheck"), source: wgpu::ShaderSource::Wgsl(MIP_SHADER.into()) });
	let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
		binding,
		visibility: wgpu::ShaderStages::FRAGMENT,
		ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
		count: None,
	};
	let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
		label: Some("mipcheck"),
		entries: &[tex_entry(0), tex_entry(1), wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None }],
	});
	let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("mipcheck"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
	let fmt = wgpu::TextureFormat::Rgba8Unorm;
	let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
		label: Some("mipcheck"),
		layout: Some(&layout),
		vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_mip"), compilation_options: Default::default(), buffers: &[] },
		primitive: Default::default(),
		depth_stencil: None,
		multisample: Default::default(),
		fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs_mip"), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: fmt, blend: None, write_mask: wgpu::ColorWrites::ALL })] }),
		multiview_mask: None,
		cache: None,
	});
	let mip_tex = |w: u32, h: u32, levels: u32| {
		let t = device.create_texture(&wgpu::TextureDescriptor {
			label: Some("mipcheck"),
			size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
			mip_level_count: levels,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: fmt,
			usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
			view_formats: &[],
		});
		for l in 0..levels {
			let (lw, lh) = ((w >> l).max(1), (h >> l).max(1));
			let data: Vec<u8> = (0..lw * lh).flat_map(|_| level_colour(l)).collect();
			queue.write_texture(
				wgpu::TexelCopyTextureInfo { texture: &t, mip_level: l, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
				&data,
				wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(lw * 4), rows_per_image: None },
				wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 },
			);
		}
		t
	};
	let (ta, tb) = (mip_tex(256, 256, 9), mip_tex(64, 48, 7));
	let (va, vb) = (ta.create_view(&Default::default()), tb.create_view(&Default::default()));
	let smp = device.create_sampler(&wgpu::SamplerDescriptor { mipmap_filter: wgpu::MipmapFilterMode::Nearest, ..Default::default() });
	let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
		label: Some("mipcheck"),
		layout: &bgl,
		entries: &[
			wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&va) },
			wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&vb) },
			wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&smp) },
		],
	});
	// Indirect arguments: two plain draws (6 vertices, 1 instance, first vertex 0, first instance 0 / 1), then one indexed
	// draw (6 indices, 1 instance, first index 0, base vertex 0, first instance 2).
	let args: [u32; 13] = [6, 1, 0, 0, 6, 1, 0, 1, 6, 1, 0, 0, 2];
	let ind = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mipcheck args"), size: 64, usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
	queue.write_buffer(&ind, 0, bytes(&args));
	let ib = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mipcheck ib"), size: 12, usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
	queue.write_buffer(&ib, 0, bytes(&[0u16, 1, 2, 3, 4, 5]));
	let target = |w: u32, h: u32, format: wgpu::TextureFormat| {
		device.create_texture(&wgpu::TextureDescriptor {
			label: Some("mipcheck target"),
			size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
			view_formats: &[],
		})
	};
	let (out, half) = (target(16, 3, fmt), target(4, 1, wgpu::TextureFormat::Rgba16Float));
	let (out_v, half_v) = (out.create_view(&Default::default()), half.create_view(&Default::default()));
	let rb = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mipcheck readback"), size: 256 * 4, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
	let mut enc = device.create_command_encoder(&Default::default());
	let attach = |v, c| wgpu::RenderPassColorAttachment { view: v, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(c), store: wgpu::StoreOp::Store } };
	{
		let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
			label: Some("mipcheck"),
			color_attachments: &[Some(attach(&out_v, wgpu::Color::BLACK))],
			depth_stencil_attachment: None,
			timestamp_writes: None,
			occlusion_query_set: None,
			multiview_mask: None,
		});
		pass.set_pipeline(&pipe);
		pass.set_bind_group(0, &bind, &[]);
		pass.multi_draw_indirect(&ind, 0, 2);
		pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
		pass.draw_indexed_indirect(&ind, 32);
	}
	{
		let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
			label: Some("half clear"),
			color_attachments: &[Some(attach(&half_v, wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }))],
			depth_stencil_attachment: None,
			timestamp_writes: None,
			occlusion_query_set: None,
			multiview_mask: None,
		});
	}
	let layout = |offset| wgpu::TexelCopyBufferLayout { offset, bytes_per_row: Some(256), rows_per_image: None };
	enc.copy_texture_to_buffer(out.as_image_copy(), wgpu::TexelCopyBufferInfo { buffer: &rb, layout: layout(0) }, wgpu::Extent3d { width: 16, height: 3, depth_or_array_layers: 1 });
	enc.copy_texture_to_buffer(half.as_image_copy(), wgpu::TexelCopyBufferInfo { buffer: &rb, layout: layout(768) }, wgpu::Extent3d { width: 4, height: 1, depth_or_array_layers: 1 });
	queue.submit([enc.finish()]);
	rb.slice(..).map_async(wgpu::MapMode::Read, |_| {});
	let _ = device.poll(wgpu::PollType::wait_indefinitely());
	let Ok(data) = rb.slice(..).get_mapped_range() else {
		say(&format!("wgputest: check {tag}: readback not mapped"));
		return false;
	};
	let mut all = true;
	for (row, levels) in [(0u32, 9u32), (1, 7), (2, 9)] {
		let mut got = String::new();
		let mut ok = true;
		for x in 0..16u32 {
			let o = (row * 256 + x * 4) as usize;
			let px = [data[o], data[o + 1], data[o + 2], data[o + 3]];
			let want = level_colour(x.min(levels - 1));
			ok &= px == want;
			got += &format!(" {:02x}{:02x}{:02x}", px[0], px[1], px[2]);
		}
		all &= ok;
		let what = ["A, multi-draw-indirect", "B (64x48), multi-draw-indirect first instance 1", "A, indexed indirect first instance 2"][row as usize];
		say(&format!("wgputest: check {tag}: mip row {row} ({what}): {} -{got}", if ok { "PASS" } else { "FAIL" }));
	}
	let half_px = &data[768..768 + 32];
	let ok = half_px.chunks(8).all(|t| t == [0, 0, 0, 0, 0, 0, 0, 0x3c]);
	all &= ok;
	say(&format!("wgputest: check {tag}: Rgba16Float clear (0,0,0,1): {} - {:02x?}", if ok { "PASS" } else { "FAIL" }, &half_px[..8]));
	drop(data);
	rb.unmap();
	all
}

static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);

struct Logger;

impl log::Log for Logger {
	fn enabled(&self, m: &log::Metadata) -> bool {
		m.level() <= log::Level::Info || m.target().starts_with("wgpu_hal")
	}
	fn log(&self, r: &log::Record) {
		if !self.enabled(r.metadata()) || r.level() > log::Level::Info {
			return;
		}
		say(&format!("[{} {}] {}", r.level(), r.target(), r.args()));
	}
	fn flush(&self) {}
}

fn say(s: &str) {
	if let Some(f) = LOG.lock().unwrap().as_mut() {
		let _ = writeln!(f, "{s}");
		let _ = f.flush();
	}
	if cfg!(not(target_os = "freebsd")) {
		eprintln!("{s}");
	}
}

/// Runs a future that wgpu resolves at once on native platforms.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
	let mut f = std::pin::pin!(f);
	let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
	loop {
		if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
			return v;
		}
		std::thread::yield_now();
	}
}

fn main() {
	#[cfg(target_os = "freebsd")]
	{
		rac_ps4_shim::init();
		let _ = std::fs::create_dir_all("/data/rac-rs");
		*LOG.lock().unwrap() = std::fs::File::create("/data/rac-rs/wgputest.log").ok();
		std::panic::set_hook(Box::new(|p| say(&format!("wgputest: PANIC {p}"))));
	}
	let _ = log::set_logger(&Logger);
	log::set_max_level(log::LevelFilter::Info);
	run();
	say("wgputest: done");
	#[cfg(target_os = "freebsd")]
	rac_ps4_shim::quit(0);
}

/// Column-major 4x4 product a * b.
fn mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
	let mut r = [0f32; 16];
	for c in 0..4 {
		for row in 0..4 {
			r[c * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[c * 4 + k]).sum();
		}
	}
	r
}

/// A turning cube 3 units in front of the camera, wgpu clip space (y up, z 0..1), depth "less".
fn mvp(t: f32) -> [f32; 16] {
	let f = 1.0 / (0.5f32).tan();
	let (near, far) = (0.1f32, 100.0f32);
	let a = W as f32 / H as f32;
	let p = [f / a, 0.0, 0.0, 0.0, 0.0, f, 0.0, 0.0, 0.0, 0.0, far / (near - far), -1.0, 0.0, 0.0, near * far / (near - far), 0.0];
	let (s1, c1) = t.sin_cos();
	let (s2, c2) = (t * 0.7).sin_cos();
	let ry = [c1, 0.0, -s1, 0.0, 0.0, 1.0, 0.0, 0.0, s1, 0.0, c1, 0.0, 0.0, 0.0, 0.0, 1.0];
	let rx = [1.0, 0.0, 0.0, 0.0, 0.0, c2, s2, 0.0, 0.0, -s2, c2, 0.0, 0.0, 0.0, 0.0, 1.0];
	let tr = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, -3.0, 1.0];
	mul(&p, &mul(&tr, &mul(&ry, &rx)))
}

/// 24 vertices (pos, uv, colour) and 36 u16 indices of a unit cube, counter-clockwise outside.
fn cube() -> (Vec<f32>, Vec<u16>) {
	let faces: [([f32; 3], [f32; 3], [f32; 3], [f32; 3]); 6] = [
		([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.3, 0.3]),
		([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.3, 1.0, 0.3]),
		([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [0.3, 0.3, 1.0]),
		([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.3]),
		([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.3, 1.0]),
		([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.3, 1.0, 1.0]),
	];
	let (mut v, mut idx) = (Vec::new(), Vec::new());
	for (n, u, w, c) in faces {
		let base = (v.len() / 8) as u16;
		for (a, b) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
			for k in 0..3 {
				v.push(0.5 * (n[k] + a * u[k] + b * w[k]));
			}
			v.extend_from_slice(&[(a + 1.0) * 0.5, (1.0 - b) * 0.5]);
			v.extend_from_slice(&c);
		}
		idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
	}
	(v, idx)
}

fn bytes<T: Copy>(v: &[T]) -> &[u8] {
	// SAFETY: plain numbers.
	unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn run() {
	let gnm = cfg!(target_os = "freebsd") || std::env::var("RAC_WGPU").is_ok_and(|v| v == "gnm");
	let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
	if gnm {
		desc.backends = wgpu::Backends::NOOP;
		desc.backend_options.noop.enable = true;
	} else {
		desc.backends = wgpu::Backends::VULKAN;
	}
	let instance = wgpu::Instance::new(desc);
	// The PS4 has one display; the GNM back end takes any handle as it.
	let surface = if cfg!(target_os = "freebsd") {
		let win = raw_window_handle::RawWindowHandle::Web(raw_window_handle::WebWindowHandle::new(1));
		let disp = raw_window_handle::RawDisplayHandle::Web(raw_window_handle::WebDisplayHandle::new());
		// SAFETY: the handles are never dereferenced by the GNM back end.
		match unsafe { instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle { raw_display_handle: Some(disp), raw_window_handle: win }) } {
			Ok(s) => Some(s),
			Err(e) => {
				say(&format!("wgputest: create_surface failed: {e}"));
				None
			}
		}
	} else {
		None
	};
	let adapter = match block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { compatible_surface: surface.as_ref(), ..Default::default() })) {
		Ok(a) => a,
		Err(e) => {
			say(&format!("wgputest: no adapter: {e}"));
			return;
		}
	};
	say(&format!("wgputest: adapter {:?}", adapter.get_info().name));
	let (device, queue) = match block_on(adapter.request_device(&wgpu::DeviceDescriptor {
		label: Some("wgputest"),
		required_features: wgpu::Features::IMMEDIATES | wgpu::Features::INDIRECT_FIRST_INSTANCE,
		required_limits: wgpu::Limits { max_immediate_size: 16, ..wgpu::Limits::default() },
		..Default::default()
	})) {
		Ok(d) => d,
		Err(e) => {
			say(&format!("wgputest: request_device failed: {e}"));
			return;
		}
	};
	device.on_uncaptured_error(std::sync::Arc::new(|e| say(&format!("wgputest: wgpu error: {e}"))));
	if let Some(s) = &surface {
		s.configure(&device, &wgpu::SurfaceConfiguration {
			usage: wgpu::TextureUsages::COPY_DST,
			format: FORMAT,
			width: W,
			height: H,
			present_mode: wgpu::PresentMode::Fifo,
			desired_maximum_frame_latency: 1,
			alpha_mode: wgpu::CompositeAlphaMode::Opaque,
			color_space: Default::default(),
			view_formats: vec![],
		});
	}

	// Where the hardware reads each mip level from (run 4 showed rule a right except a 2x2 level), then the mip /
	// indirect / clear check with the back end's current rule.
	probe(&device, &queue);
	let ok = checks(&device, &queue, "mips");
	say(&format!("wgputest: checks: {}", if ok { "PASS" } else { "FAIL" }));

	let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("wgputest"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
	let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
		label: Some("cube"),
		entries: &[
			wgpu::BindGroupLayoutEntry {
				binding: 0,
				visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
				ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
				count: None,
			},
			wgpu::BindGroupLayoutEntry {
				binding: 1,
				visibility: wgpu::ShaderStages::FRAGMENT,
				ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
				count: None,
			},
			wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
		],
	});
	let cube_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("cube"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
	let bg_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("bg"), bind_group_layouts: &[], immediate_size: 16 });
	let target = [Some(wgpu::ColorTargetState { format: FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })];
	let depth_format = wgpu::TextureFormat::Depth32Float;
	let cube_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
		label: Some("cube"),
		layout: Some(&cube_layout),
		vertex: wgpu::VertexState {
			module: &module,
			entry_point: Some("vs_cube"),
			compilation_options: Default::default(),
			buffers: &[Some(wgpu::VertexBufferLayout {
				array_stride: 32,
				step_mode: wgpu::VertexStepMode::Vertex,
				attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x3],
			})],
		},
		primitive: wgpu::PrimitiveState { cull_mode: Some(wgpu::Face::Back), ..Default::default() },
		depth_stencil: Some(wgpu::DepthStencilState {
			format: depth_format,
			depth_write_enabled: Some(true),
			depth_compare: Some(wgpu::CompareFunction::Less),
			stencil: Default::default(),
			bias: Default::default(),
		}),
		multisample: Default::default(),
		fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs_cube"), compilation_options: Default::default(), targets: &target }),
		multiview_mask: None,
		cache: None,
	});
	let bg_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
		label: Some("bg"),
		layout: Some(&bg_layout),
		vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_bg"), compilation_options: Default::default(), buffers: &[] },
		primitive: Default::default(),
		depth_stencil: Some(wgpu::DepthStencilState {
			format: depth_format,
			depth_write_enabled: Some(false),
			depth_compare: Some(wgpu::CompareFunction::Always),
			stencil: Default::default(),
			bias: Default::default(),
		}),
		multisample: Default::default(),
		fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs_bg"), compilation_options: Default::default(), targets: &target }),
		multiview_mask: None,
		cache: None,
	});

	// Resources.
	let (verts, idx) = cube();
	let vb = device.create_buffer(&wgpu::BufferDescriptor { label: Some("vb"), size: (verts.len() * 4) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
	queue.write_buffer(&vb, 0, bytes(&verts));
	let ib = device.create_buffer(&wgpu::BufferDescriptor { label: Some("ib"), size: (idx.len() * 2).next_multiple_of(4) as u64, usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
	let mut idx_padded = idx.clone();
	if idx_padded.len() % 2 == 1 {
		idx_padded.push(0);
	}
	queue.write_buffer(&ib, 0, bytes(&idx_padded));
	let ub = device.create_buffer(&wgpu::BufferDescriptor { label: Some("ub"), size: 80, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
	let (tw, th) = (256u32, 256u32);
	let texels: Vec<u8> = (0..tw * th)
		.flat_map(|i| {
			let (x, y) = (i % tw, i / tw);
			if ((x / 32) + (y / 32)) % 2 == 0 { [255u8, 255, 255, 255] } else { [90, 90, 90, 255] }
		})
		.collect();
	let tex = device.create_texture(&wgpu::TextureDescriptor {
		label: Some("checks"),
		size: wgpu::Extent3d { width: tw, height: th, depth_or_array_layers: 1 },
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format: wgpu::TextureFormat::Rgba8UnormSrgb,
		usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
		view_formats: &[],
	});
	queue.write_texture(
		wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
		&texels,
		wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(tw * 4), rows_per_image: None },
		wgpu::Extent3d { width: tw, height: th, depth_or_array_layers: 1 },
	);
	let smp = device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
	let tex_view = tex.create_view(&Default::default());
	let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
		label: Some("cube"),
		layout: &bgl,
		entries: &[
			wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() },
			wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&tex_view) },
			wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&smp) },
		],
	});
	let color = device.create_texture(&wgpu::TextureDescriptor {
		label: Some("colour"),
		size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format: FORMAT,
		usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
		view_formats: &[],
	});
	let color_view = color.create_view(&Default::default());
	let depth = device.create_texture(&wgpu::TextureDescriptor {
		label: Some("depth"),
		size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format: depth_format,
		usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
		view_formats: &[],
	});
	let depth_view = depth.create_view(&Default::default());
	// Readback of a few pixels' rows.
	let row_bytes = W * 4;
	let readback = device.create_buffer(&wgpu::BufferDescriptor { label: Some("readback"), size: (row_bytes * H) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });

	let t0 = std::time::Instant::now();
	for frame in 0..FRAMES {
		let mut u = [0f32; 20];
		u[..16].copy_from_slice(&mvp(frame as f32 * 0.02));
		u[16..].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
		queue.write_buffer(&ub, 0, bytes(&u));
		let mut enc = device.create_command_encoder(&Default::default());
		{
			let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("frame"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &color_view,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }), store: wgpu::StoreOp::Store },
				})],
				depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
					view: &depth_view,
					depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
					stencil_ops: None,
				}),
				timestamp_writes: None,
				occlusion_query_set: None,
				multiview_mask: None,
			});
			pass.set_pipeline(&bg_pipe);
			pass.set_immediates(0, bytes(&[0.2f32, 0.25, 0.6, 1.0]));
			pass.draw(0..3, 0..1);
			pass.set_pipeline(&cube_pipe);
			pass.set_bind_group(0, &bind, &[]);
			pass.set_vertex_buffer(0, vb.slice(..));
			pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
			pass.draw_indexed(0..idx.len() as u32, 0, 0..1);
		}
		let check = frame == 0 || frame == 1 || frame == 120;
		if check {
			enc.copy_texture_to_buffer(
				wgpu::TexelCopyTextureInfo { texture: &color, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
				wgpu::TexelCopyBufferInfo { buffer: &readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row_bytes), rows_per_image: None } },
				wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
			);
		}
		let frame_tex = surface.as_ref().and_then(|s| match s.get_current_texture() {
			wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => Some(t),
			other => {
				if frame < 2 {
					say(&format!("wgputest: no surface texture: {other:?}"));
				}
				None
			}
		});
		if let Some(t) = &frame_tex {
			enc.copy_texture_to_texture(
				wgpu::TexelCopyTextureInfo { texture: &color, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
				wgpu::TexelCopyTextureInfo { texture: &t.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
				wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
			);
		}
		queue.submit([enc.finish()]);
		if check {
			readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
			let _ = device.poll(wgpu::PollType::wait_indefinitely());
			{
				let Ok(data) = readback.slice(..).get_mapped_range() else {
					say("wgputest: readback not mapped");
					continue;
				};
				let px = |x: u32, y: u32| {
					let o = ((y * W + x) * 4) as usize;
					u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
				};
				say(&format!(
					"wgputest: frame {frame}: pixels (B G R A bytes as a word) corner {:08x}, top {:08x}, centre {:08x}, near centre {:08x}",
					px(8, 8),
					px(W / 2, 40),
					px(W / 2, H / 2),
					px(W / 2 + 60, H / 2 - 60)
				));
			}
			readback.unmap();
		}
		if let Some(t) = frame_tex {
			queue.present(t);
		}
		if frame % 120 == 119 {
			say(&format!("wgputest: {} frames in {:.1} s", frame + 1, t0.elapsed().as_secs_f32()));
		}
		if !cfg!(target_os = "freebsd") && frame >= 2 && std::env::var("RAC_WGPU_FRAMES").is_err() {
			// On the PC three frames show everything (capture, cache check, pixels).
			break;
		}
	}
	say(&format!("wgputest: RESULT: finished, {:.1} s", t0.elapsed().as_secs_f32()));
}
