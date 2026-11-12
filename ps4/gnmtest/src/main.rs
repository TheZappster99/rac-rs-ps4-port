//! PS4 port, GNM test 2: shaders written in WGSL (shaders/test.wgsl), compiled for the PS4's GPU by Mesa radv on the PC, drawn
//! on the console through the GNM layer. This is how rac-rs's renderer would run (PS4_GPU_DESIGN.md, option B), on a small scale:
//!   - a background triangle sampling a texture through descriptor set 0 (texture, sampler and uniform buffer descriptors at
//!     the offsets radv's set layout gives them), tinted by the uniform buffer, writing depth "far" (the depth clear);
//!   - a spinning cube from two vertex buffers (radv's vertex-buffer descriptor table), its matrix from the uniform buffer,
//!     depth-tested against the background.
//! radv's GFX7 shaders read descriptor sets and the vertex-buffer table through 32-bit pointers (high half 0), so those live in
//! an arena mapped below 4 GB; whether the console allows such a mapping is the first thing this test finds out.
//! Everything goes to /data/rac-rs/gnmtest.log, with pixels read back after the first frame.

use std::io::Write;

use rac_ps4_shim as _;

const LOG: &str = "/data/rac-rs/gnmtest.log";

struct Log(Option<std::fs::File>);

impl Log {
	fn say(&mut self, s: impl AsRef<str>) {
		if let Some(f) = self.0.as_mut() {
			let _ = writeln!(f, "{}", s.as_ref());
			let _ = f.flush();
		}
	}
}

macro_rules! rsh {
	($name:literal) => {
		include_bytes!(concat!(env!("RAC_PS4_RSH"), "/test.", $name, ".rsh"))
	};
}

static BG_VS: &[u8] = rsh!("bg_vs");
static BG_FS: &[u8] = rsh!("bg_fs");
static CUBE_VS: &[u8] = rsh!("cube_vs");
static CUBE_FS: &[u8] = rsh!("cube_fs");

fn main() {
	#[cfg(target_os = "freebsd")]
	{
		rac_ps4_shim::init();
		let _ = std::fs::create_dir_all("/data/rac-rs");
		let mut log = Log(std::fs::File::create(LOG).ok());
		let lp = LOG.to_string();
		std::panic::set_hook(Box::new(move |p| {
			if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&lp) {
				let _ = writeln!(f, "gnmtest: PANIC {p}");
			}
		}));
		run(&mut log);
		log.say("gnmtest: done");
		rac_ps4_shim::quit(0);
	}
	#[cfg(not(target_os = "freebsd"))]
	{
		// On the PC (prints, writes no file): check the shader entries parse and show what the console run will use.
		let _ = Log(None);
		for (n, b) in [("bg_vs", BG_VS), ("bg_fs", BG_FS), ("cube_vs", CUBE_VS), ("cube_fs", CUBE_FS)] {
			match rac_ps4_gnm::shader::Shader::parse(b) {
				Ok(s) => println!("{n}: {:?}, {} B, user SGPRs {}, set 0 {:?}", s.stage, s.code.len(), s.user_sgprs(), s.set_sgpr(0)),
				Err(e) => println!("{n}: {e:?}"),
			}
		}
		let m = cube_mvp(0.0, 16.0 / 9.0);
		// The cube's centre (origin) lands at clip (0, 0, near, 3): screen centre, depth near / 3.
		println!("cube centre in clip space: ({}, {}, {}, {})", m[12], m[13], m[14], m[15]);
		println!("{} cube vertices", cube().0.len());
	}
}

/// Column-major 4x4 matrix product a * b.
fn mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
	let mut r = [0f32; 16];
	for c in 0..4 {
		for row in 0..4 {
			r[c * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[c * 4 + k]).sum();
		}
	}
	r
}

/// The cube's matrix: reversed-Z infinite perspective (Vulkan clip space, y down), the camera 3 units back, turning.
fn cube_mvp(t: f32, aspect: f32) -> [f32; 16] {
	let f = 1.0 / (0.5f32).tan();
	let near = 0.1;
	// Column-major: clip = P * view.  x' = f/a x, y' = -f y (Vulkan y points down), z' = near, w' = -z.
	let p = [f / aspect, 0.0, 0.0, 0.0, 0.0, -f, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, near, 0.0];
	let (s1, c1) = t.sin_cos();
	let (s2, c2) = (t * 0.7).sin_cos();
	let ry = [c1, 0.0, -s1, 0.0, 0.0, 1.0, 0.0, 0.0, s1, 0.0, c1, 0.0, 0.0, 0.0, 0.0, 1.0];
	let rx = [1.0, 0.0, 0.0, 0.0, 0.0, c2, s2, 0.0, 0.0, -s2, c2, 0.0, 0.0, 0.0, 0.0, 1.0];
	let tr = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, -3.0, 1.0];
	mul(&p, &mul(&tr, &mul(&ry, &rx)))
}

/// 36 vertices (12 triangles) of a unit cube, one colour per face.
fn cube() -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
	let faces: [([f32; 3], [f32; 3], [f32; 3], [f32; 3]); 6] = [
		([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.2, 0.2]),
		([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.2, 1.0, 0.2]),
		([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [0.2, 0.2, 1.0]),
		([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.2]),
		([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.2, 1.0]),
		([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.2, 1.0, 1.0]),
	];
	let (mut p, mut c) = (Vec::new(), Vec::new());
	for (n, u, v, col) in faces {
		let at = |a: f32, b: f32| [0.5 * (n[0] + a * u[0] + b * v[0]), 0.5 * (n[1] + a * u[1] + b * v[1]), 0.5 * (n[2] + a * u[2] + b * v[2])];
		for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
			p.push(at(a, b));
			c.push(col);
		}
	}
	(p, c)
}

#[cfg(target_os = "freebsd")]
fn run(log: &mut Log) {
	use rac_ps4_gnm::ps4::{self, Arena, Video};
	use rac_ps4_gnm::regs::{self, blend, db_depth_control, Compare};
	use rac_ps4_gnm::shader::{bind_graphics, ud, Shader};
	use rac_ps4_gnm::{desc, pm4::Cmd, target};

	let (w, h) = (1920u32, 1080u32);
	let shaders: Vec<Shader> = match [BG_VS, BG_FS, CUBE_VS, CUBE_FS].iter().map(|b| Shader::parse(b)).collect() {
		Ok(v) => v,
		Err(e) => {
			log.say(format!("gnmtest: bad shader entry: {e:?}"));
			return;
		}
	};
	let (bg_vs, bg_fs, cube_vs, cube_fs) = (&shaders[0], &shaders[1], &shaders[2], &shaders[3]);
	for (n, s) in [("bg_vs", bg_vs), ("bg_fs", bg_fs), ("cube_vs", cube_vs), ("cube_fs", cube_fs)] {
		log.say(format!("gnmtest: {n}: {} B code, {} user SGPRs, set 0 at {:?}, rsrc1 {:#x} rsrc2 {:#x}", s.code.len(), s.user_sgprs(), s.set_sgpr(0), s.rsrc1, s.rsrc2));
	}

	// Memory: one big arena anywhere, and a small one below 4 GB for what shaders reach through 32-bit pointers.
	let rt_bytes = (w * h * 4) as usize;
	let mut mem = match Arena::new(3 * rt_bytes + target::depth_bytes(w, h) + (8 << 20)) {
		Ok(a) => a,
		Err(e) => {
			log.say(format!("gnmtest: main arena: {e}"));
			return;
		}
	};
	log.say(format!("gnmtest: main arena {} MB at {:p}", mem.len >> 20, mem.base));
	let mut low = None;
	for at in [0x8000_0000u64, 0x4000_0000, 0xc000_0000, 0x2000_0000, 0x1000_0000, 0x6000_0000, 0xa000_0000, 0xe000_0000] {
		match Arena::new_at(2 << 20, at) {
			Ok(a) => {
				log.say(format!("gnmtest: low arena (32-bit pointers) mapped at {at:#x}"));
				low = Some(a);
				break;
			}
			Err(e) => log.say(format!("gnmtest: low arena at {at:#x} refused: {e}")),
		}
	}
	if low.is_none() {
		for at in [0x8000_0000u64, 0x4000_0000, 0x1000_0000] {
			match Arena::new_below(2 << 20, at, 1 << 32) {
				Ok(a) => {
					log.say(format!("gnmtest: low arena (32-bit pointers) mapped by hint {at:#x} at {:p}", a.base));
					low = Some(a);
					break;
				}
				Err(e) => log.say(format!("gnmtest: low arena by hint: {e}")),
			}
		}
	}
	let Some(mut low) = low else {
		log.say("gnmtest: RESULT: no memory below 4 GB; radv's GFX7 shaders cannot find their descriptors. Stopping before the GPU work.");
		return;
	};

	// Render targets (the video out buffers) and depth.
	let rts: Vec<*mut u8> = (0..3).map(|_| mem.alloc(rt_bytes, 64 << 10)).collect();
	for &p in &rts {
		// Magenta: whatever the GPU leaves untouched shows.
		// SAFETY: a fresh render target of w*h pixels.
		unsafe { core::slice::from_raw_parts_mut(p as *mut u32, (w * h) as usize).fill(0xffff_00ff) };
	}
	let depth = mem.alloc(target::depth_bytes(w, h), 64 << 10) as u64;
	let video = match Video::open(&rts, w, h, w) {
		Ok(v) => v,
		Err(e) => {
			log.say(format!("gnmtest: video out: {e}"));
			return;
		}
	};

	// Shader code, 256-byte aligned.
	let code = |m: &mut Arena, s: &Shader| m.upload(s.code, 256);
	let (bg_vs_at, bg_fs_at, cube_vs_at, cube_fs_at) = (code(&mut mem, bg_vs), code(&mut mem, bg_fs), code(&mut mem, cube_vs), code(&mut mem, cube_fs));

	// A 256 x 256 RGBA8 texture: coloured checks with a gradient.
	let (tw, th) = (256u32, 256u32);
	let texels: Vec<u8> = (0..tw * th)
		.flat_map(|i| {
			let (x, y) = (i % tw, i / tw);
			let check = ((x / 32) + (y / 32)) % 2 == 0;
			if check { [x as u8, y as u8, 200, 255] } else { [30, 30, 40, 255] }
		})
		.collect();
	let tex = mem.upload(&texels, 256);

	// Vertex buffers and uniform buffers (anywhere: their addresses sit in 64-bit-capable descriptors).
	let (cp, cc) = cube();
	let as_bytes = |v: &[[f32; 3]]| v.iter().flatten().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
	let vb_pos = mem.upload(&as_bytes(&cp), 256);
	let vb_col = mem.upload(&as_bytes(&cc), 256);
	let ubo = mem.alloc(256, 256);
	// The scratch-ring table radv's shaders get in user SGPRs 0-1 (none of these use scratch): zeros.
	let ring = mem.upload(&[0u8; 256], 256);
	// Labels, one per checkpoint of a frame: after the default state (+ cache invalidate), after the background, after the
	// cube, and the frame's end. When the GPU hangs, the ones that got the frame's number show how far it went.
	let labels = mem.alloc(64, 64) as *mut u32;
	// SAFETY: four fresh label words.
	unsafe { core::ptr::write_bytes(labels, 0, 4) };
	// Descriptor sets and the vertex-buffer table, below 4 GB. radv's layout of the background's set 0 (bindings 0 uniform,
	// 1 texture, 2 sampler), read from its NIR (radv.log): texture at 0 (64 bytes: T# + 32 reserved), uniform at 64, sampler
	// at 80. The cube's set 0 only has the uniform buffer, at 0.
	let bg_set = low.words(24, 256);
	bg_set[0..8].copy_from_slice(&desc::texture_2d(tex, tw, th, tw, desc::IMG_DATA_FORMAT_8_8_8_8, desc::IMG_NUM_FORMAT_UNORM, desc::TILE_DISPLAY_LINEAR_ALIGNED, 1));
	bg_set[16..20].copy_from_slice(&desc::buffer(ubo as u64, 0, 80));
	bg_set[20..24].copy_from_slice(&desc::sampler(desc::WRAP_REPEAT, desc::FILTER_BILINEAR));
	let cube_set = low.words(4, 256);
	cube_set.copy_from_slice(&desc::buffer(ubo as u64, 0, 80));
	let vb_table = low.words(8, 256);
	vb_table[0..4].copy_from_slice(&desc::buffer(vb_pos, 12, 36));
	vb_table[4..8].copy_from_slice(&desc::buffer(vb_col, 12, 36));
	let (bg_set_at, cube_set_at, vb_table_at) = (bg_set.as_ptr() as u64, cube_set.as_ptr() as u64, vb_table.as_ptr() as u64);
	log.say(format!("gnmtest: sets at {bg_set_at:#x} / {cube_set_at:#x}, vertex buffer table {vb_table_at:#x}"));

	let cmd_bufs = [mem.words(16 << 10, 256), mem.words(16 << 10, 256)];
	let ring_lo_hi = [ring as u32, (ring >> 32) as u32];
	let frames = 600u32;
	let mut cmd_bufs = cmd_bufs;
	// Run 1 drew frame 0 right, then hung in frame 1. The frame now grows in phases of 3 frames each, so the first phase
	// that hangs (or whose next frame hangs, if the GPU faults after the end label) names the cause.
	const PHASES: [&str; 5] = [
		"empty frame (default state + end label)",
		"+ shader cache invalidate (ACQUIRE_MEM)",
		"+ colour target, background draw (texture), no depth target",
		"+ depth target",
		"+ cube (vertex buffers, depth test): the full frame",
	];
	let label = |i: usize| unsafe { labels.add(i) };
	let mut phase_was = usize::MAX;
	for frame in 0..frames {
		let phase = ((frame / 3) as usize).min(PHASES.len() - 1);
		if phase != phase_was {
			log.say(format!("gnmtest: frame {frame}: phase {phase}: {}", PHASES[phase]));
			phase_was = phase;
		}
		let idx = (frame % 3) as usize;
		// Uniforms: the cube's matrix, the background's tint.
		let m = cube_mvp(frame as f32 * 0.02, w as f32 / h as f32);
		let tint = [0.7f32, 0.7, 0.9, 1.0];
		// SAFETY: 80 bytes of the uniform buffer; the GPU finished the previous frame (waited below).
		unsafe {
			let u = ubo as *mut f32;
			for (i, v) in m.iter().chain(tint.iter()).enumerate() {
				u.add(i).write_volatile(*v);
			}
		}
		let buf = &mut cmd_bufs[(frame % 2) as usize];
		let mut c = Cmd::new(buf);
		let r_init = ps4::init_default_state(c.reserve(256));
		if phase >= 1 {
			// The uniform buffer changed since the last frame: no stale copies in the GPU caches.
			c.invalidate_shader_caches();
		}
		c.eop_label(label(0) as u64, frame + 1);
		let (mut rv1, mut rv2, mut r_draw1, mut r_draw2) = ((0, 0), (0, 0), 0, 0);
		if phase >= 2 {
			target::color_bgra8(&mut c, rts[idx] as u64, w, h, w);
			if phase >= 3 {
				target::depth_z32f(&mut c, depth, w, h);
			} else {
				target::depth_none(&mut c);
			}
			target::viewport(&mut c, w, h);
			c.ctx(regs::CB_BLEND0_CONTROL, &[blend::control(false, 0, 0, 0, 0, 0, 0)]);
			c.ctx(regs::PA_SU_SC_MODE_CNTL, &[0]);
			c.primitive(regs::prim::TRILIST);
			c.num_instances(1);

			// Background: depth ALWAYS, writing 0 (far, reversed Z) everywhere: the depth clear.
			rv1 = bind_graphics(&mut c, bg_vs, bg_fs, ps4::set_vs, ps4::set_ps, bg_vs_at, bg_fs_at);
			c.user_vs(0, &ring_lo_hi);
			if let Some(l) = bg_vs.ud(ud::VS_BASE_VERTEX_START_INSTANCE) {
				c.user_vs(l.sgpr, &vec![0; l.count as usize]);
			}
			c.user_ps(0, &ring_lo_hi);
			if let Some(l) = bg_fs.set_sgpr(0) {
				c.user_ps(l.sgpr, &[bg_set_at as u32]);
			}
			if phase >= 3 {
				c.ctx(regs::DB_DEPTH_CONTROL, &[db_depth_control(true, true, Compare::Always)]);
			}
			r_draw1 = ps4::draw_auto(c.reserve(7), 3);
		}
		c.eop_label(label(1) as u64, frame + 1);
		if phase >= 4 {
			// Cube: depth GREATER (reversed Z), writes on.
			rv2 = bind_graphics(&mut c, cube_vs, cube_fs, ps4::set_vs, ps4::set_ps, cube_vs_at, cube_fs_at);
			c.user_vs(0, &ring_lo_hi);
			if let Some(l) = cube_vs.ud(ud::VS_VERTEX_BUFFERS) {
				c.user_vs(l.sgpr, &[vb_table_at as u32]);
			}
			if let Some(l) = cube_vs.ud(ud::VS_BASE_VERTEX_START_INSTANCE) {
				c.user_vs(l.sgpr, &vec![0; l.count as usize]);
			}
			if let Some(l) = cube_vs.set_sgpr(0) {
				c.user_vs(l.sgpr, &[cube_set_at as u32]);
			}
			c.user_ps(0, &ring_lo_hi);
			c.ctx(regs::DB_DEPTH_CONTROL, &[db_depth_control(true, true, Compare::Greater)]);
			r_draw2 = ps4::draw_auto(c.reserve(7), 36);
		}
		c.eop_label(label(2) as u64, frame + 1);
		c.eop_label(label(3) as u64, frame + 1);
		let words = c.len();
		let (r_sub, r_done) = ps4::submit(c.words());
		let first_of_phase = frame < 15 && frame % 3 == 0;
		if first_of_phase || r_sub != 0 {
			log.say(format!(
				"gnmtest: frame {frame}: {words} dwords; init {r_init:#x}, shader sets {rv1:x?} {rv2:x?}, draws {r_draw1:#x} {r_draw2:#x}, submit {r_sub:#x}, done {r_done:#x}"
			));
		}
		if r_sub != 0 {
			return;
		}
		let ok = ps4::wait_label(label(3), frame + 1, 2000);
		if frame < 15 || !ok {
			let px = |x: u32, y: u32| {
				// SAFETY: inside the w*h render target.
				unsafe { (rts[idx] as *const u32).add((y * w + x) as usize).read_volatile() }
			};
			// SAFETY: the label words.
			let got: Vec<u32> = (0..4).map(|i| unsafe { label(i).read_volatile() }).collect();
			log.say(format!(
				"gnmtest: frame {frame}: GPU {}; labels (start, bg, cube, end) {got:?} (want {}); pixels: corner {:08x}, left {:08x}, centre {:08x}, near centre {:08x}",
				if ok { "finished" } else { "DID NOT finish (2 s)" },
				frame + 1,
				px(8, 8),
				px(300, h / 2),
				px(w / 2, h / 2),
				px(w / 2 + 60, h / 2 - 60)
			));
		}
		if !ok {
			log.say(format!("gnmtest: RESULT: the GPU hung in phase {phase} ({}), frame {frame}", PHASES[phase]));
			return;
		}
		let rf = video.flip(idx, frame as i64);
		if frame < 2 {
			log.say(format!("gnmtest: frame {frame}: flip {rf:#x}"));
		}
	}
	log.say(format!("gnmtest: RESULT: {frames} frames drawn with radv-compiled WGSL shaders"));
}
