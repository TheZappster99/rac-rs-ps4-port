//! PS4 port, step 2 part 2: can the game draw with the PS4's GPU? The CPU picture (probe, GS-full) is correct but manages ~3
//! pictures a second on the console, so the playable picture needs the GPU. The only GPU API homebrew can reach without Sony's
//! SDK is Piglet, the system's OpenGL ES 2 library (the web browser's WebGL runs on it). Its catch: shaders. OpenOrbis's sample
//! only feeds it precompiled PSSL binaries borrowed from WebKit. This test finds out:
//!
//! 1. Whether Piglet loads and makes a context (it should: the OpenOrbis sample does it).
//! 2. Whether ordinary GLSL ES shaders compile at run time, with or without the system's shader compiler module
//!    (libSceShaccVSH.sprx) loaded first. Without that, every shader the renderer needs would have to come from elsewhere.
//! 3. How fast PS2-like drawing is: a render-to-texture pass, then thousands of small textured, blended triangles, in one
//!    draw call or split into many calls with texture and blend changes between them (the GS changes state constantly).
//!
//! Everything goes to /data/rac-rs/gpu.log. GL / EGL are linked directly (libScePigletv2VSH), as OpenOrbis's sample does: the
//! first run looked them up with sceKernelDlsym instead, and eglGetDisplay / eglInitialize failed.

use core::ffi::{c_char, c_void, CStr};
use std::io::Write;
use std::time::Instant;

use rac_ps4_shim as _;

const LOG: &str = "/data/rac-rs/gpu.log";

unsafe extern "C" {
	fn sceKernelGetFsSandboxRandomWord() -> *const c_char;
	fn sceKernelLoadStartModule(path: *const c_char, argc: usize, argv: *const c_void, flags: u32, opt: *mut c_void, res: *mut i32) -> i32;
	fn sceKernelAvailableFlexibleMemorySize(size: *mut usize) -> i32;
}

type Enum = u32;

/// The GL / EGL functions the test uses, imported from the Piglet module (linked with -lScePigletv2VSH).
macro_rules! gl_fns {
	($($name:ident : fn($($a:ty),*) $(-> $r:ty)?;)*) => {
		#[allow(non_snake_case)]
		mod imports {
			#[allow(unused_imports)]
			use super::*;
			unsafe extern "C" { $(pub fn $name($(_: $a),*) $(-> $r)?;)* }
		}
		#[allow(non_snake_case)]
		struct Gl { $($name: unsafe extern "C" fn($($a),*) $(-> $r)?,)* }
		impl Gl {
			fn load() -> Gl {
				Gl { $($name: imports::$name,)* }
			}
		}
	};
}

gl_fns! {
	scePigletSetConfigurationVSH: fn(*const PglConfig) -> bool;
	eglGetDisplay: fn(i32) -> *mut c_void;
	eglInitialize: fn(*mut c_void, *mut i32, *mut i32) -> u32;
	eglBindAPI: fn(Enum) -> u32;
	eglSwapInterval: fn(*mut c_void, i32) -> u32;
	eglChooseConfig: fn(*mut c_void, *const i32, *mut *mut c_void, i32, *mut i32) -> u32;
	eglCreateWindowSurface: fn(*mut c_void, *mut c_void, *const PglWindow, *const i32) -> *mut c_void;
	eglCreateContext: fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void;
	eglMakeCurrent: fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32;
	eglSwapBuffers: fn(*mut c_void, *mut c_void) -> u32;
	eglGetError: fn() -> i32;
	glGetString: fn(Enum) -> *const c_char;
	glGetError: fn() -> Enum;
	glGetBooleanv: fn(Enum, *mut u8);
	glGetIntegerv: fn(Enum, *mut i32);
	glCreateShader: fn(Enum) -> u32;
	glShaderSource: fn(u32, i32, *const *const c_char, *const i32);
	glCompileShader: fn(u32);
	glGetShaderiv: fn(u32, Enum, *mut i32);
	glGetShaderInfoLog: fn(u32, i32, *mut i32, *mut c_char);
	glCreateProgram: fn() -> u32;
	glAttachShader: fn(u32, u32);
	glBindAttribLocation: fn(u32, u32, *const c_char);
	glLinkProgram: fn(u32);
	glGetProgramiv: fn(u32, Enum, *mut i32);
	glGetProgramInfoLog: fn(u32, i32, *mut i32, *mut c_char);
	glUseProgram: fn(u32);
	glGetUniformLocation: fn(u32, *const c_char) -> i32;
	glUniform1i: fn(i32, i32);
	glGenTextures: fn(i32, *mut u32);
	glBindTexture: fn(Enum, u32);
	glTexImage2D: fn(Enum, i32, i32, i32, i32, i32, Enum, Enum, *const c_void);
	glTexParameteri: fn(Enum, Enum, i32);
	glActiveTexture: fn(Enum);
	glGenBuffers: fn(i32, *mut u32);
	glBindBuffer: fn(Enum, u32);
	glBufferData: fn(Enum, isize, *const c_void, Enum);
	glVertexAttribPointer: fn(u32, i32, Enum, u8, i32, *const c_void);
	glEnableVertexAttribArray: fn(u32);
	glDrawArrays: fn(Enum, i32, i32);
	glViewport: fn(i32, i32, i32, i32);
	glClearColor: fn(f32, f32, f32, f32);
	glClear: fn(u32);
	glEnable: fn(Enum);
	glDisable: fn(Enum);
	glBlendFunc: fn(Enum, Enum);
	glGenFramebuffers: fn(i32, *mut u32);
	glBindFramebuffer: fn(Enum, u32);
	glFramebufferTexture2D: fn(Enum, Enum, Enum, u32, i32);
	glCheckFramebufferStatus: fn(Enum) -> Enum;
	glFinish: fn();
}

/// Piglet's configuration (OpenOrbis Pigletv2VSH.h: 0x88 bytes, fields at fixed offsets).
#[repr(C, align(8))]
struct PglConfig([u8; 0x88]);

impl PglConfig {
	fn set32(&mut self, off: usize, v: u32) {
		self.0[off..off + 4].copy_from_slice(&v.to_le_bytes());
	}
	fn set64(&mut self, off: usize, v: u64) {
		self.0[off..off + 8].copy_from_slice(&v.to_le_bytes());
	}
}

#[repr(C)]
struct PglWindow {
	id: u32,
	w: u32,
	h: u32,
	pad: u32,
}

// EGL / GL constants (Khronos headers).
const EGL_NONE: i32 = 0x3038;
const EGL_OPENGL_ES_API: Enum = 0x30a0;
const GL_VENDOR: Enum = 0x1f00;
const GL_RENDERER: Enum = 0x1f01;
const GL_VERSION: Enum = 0x1f02;
const GL_EXTENSIONS: Enum = 0x1f03;
const GL_SHADING_LANGUAGE_VERSION: Enum = 0x8b8c;
const GL_SHADER_COMPILER: Enum = 0x8dfa;
const GL_NUM_SHADER_BINARY_FORMATS: Enum = 0x8df9;
const GL_MAX_TEXTURE_SIZE: Enum = 0x0d33;
const GL_VERTEX_SHADER: Enum = 0x8b31;
const GL_FRAGMENT_SHADER: Enum = 0x8b30;
const GL_COMPILE_STATUS: Enum = 0x8b81;
const GL_LINK_STATUS: Enum = 0x8b82;
const GL_TEXTURE_2D: Enum = 0x0de1;
const GL_RGBA: Enum = 0x1908;
const GL_UNSIGNED_BYTE: Enum = 0x1401;
const GL_FLOAT: Enum = 0x1406;
const GL_TEXTURE_MIN_FILTER: Enum = 0x2801;
const GL_TEXTURE_MAG_FILTER: Enum = 0x2800;
const GL_TEXTURE_WRAP_S: Enum = 0x2802;
const GL_TEXTURE_WRAP_T: Enum = 0x2803;
const GL_NEAREST: i32 = 0x2600;
const GL_CLAMP_TO_EDGE: i32 = 0x812f;
const GL_TEXTURE0: Enum = 0x84c0;
const GL_ARRAY_BUFFER: Enum = 0x8892;
const GL_STREAM_DRAW: Enum = 0x88e0;
const GL_TRIANGLES: Enum = 4;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_BLEND: Enum = 0x0be2;
const GL_SRC_ALPHA: Enum = 0x0302;
const GL_ONE_MINUS_SRC_ALPHA: Enum = 0x0303;
const GL_ONE: Enum = 1;
const GL_FRAMEBUFFER: Enum = 0x8d40;
const GL_COLOR_ATTACHMENT0: Enum = 0x8ce0;
const GL_FRAMEBUFFER_COMPLETE: Enum = 0x8cd5;

/// The screen the test draws to.
const W: i32 = 1920;
const H: i32 = 1080;

/// The PS2's usual draw: texture modulated by the vertex colour, where 0x80 is 1.0 (hence the 2x).
const VS: &str = "attribute vec2 a_pos;\nattribute vec2 a_uv;\nattribute vec4 a_col;\nvarying vec2 v_uv;\nvarying vec4 v_col;\n\
void main() { v_uv = a_uv; v_col = a_col; gl_Position = vec4(a_pos, 0.0, 1.0); }\n";
const FS: &str = "precision mediump float;\nuniform sampler2D s_tex;\nvarying vec2 v_uv;\nvarying vec4 v_col;\n\
void main() { gl_FragColor = texture2D(s_tex, v_uv) * v_col * 2.0; }\n";

/// One vertex: position (NDC), texture coordinate, colour (0x80 = 1.0 after the shader's 2x).
#[repr(C)]
#[derive(Clone, Copy)]
struct Vtx {
	x: f32,
	y: f32,
	u: f32,
	v: f32,
	rgba: [u8; 4],
}

fn flex() -> usize {
	let mut f = 0usize;
	// SAFETY: out-parameter call.
	unsafe { sceKernelAvailableFlexibleMemorySize(&mut f) };
	f >> 20
}

struct Log(Option<std::fs::File>);

impl Log {
	fn say(&mut self, s: impl AsRef<str>) {
		if let Some(f) = self.0.as_mut() {
			let _ = writeln!(f, "{}", s.as_ref());
			let _ = f.flush();
		}
	}
}

/// Loads and starts a system module; returns its handle or the error.
fn load(path: &str) -> Result<i32, String> {
	let c = std::ffi::CString::new(path).unwrap();
	let mut res = 0i32;
	// SAFETY: NUL-terminated path, no arguments.
	let h = unsafe { sceKernelLoadStartModule(c.as_ptr(), 0, core::ptr::null(), 0, core::ptr::null_mut(), &mut res) };
	if (h as i32) < 0 { Err(format!("{:#x}", h as u32)) } else { Ok(h as i32) }
}

fn main() {
	rac_ps4_shim::init();
	let _ = std::fs::create_dir_all("/data/rac-rs");
	let mut log = Log(std::fs::File::create(LOG).ok());
	let lp = LOG.to_string();
	std::panic::set_hook(Box::new(move |p| {
		if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&lp) {
			let _ = writeln!(f, "gpu: PANIC {p}");
		}
	}));
	run(&mut log);
	log.say("gpu: done");
	rac_ps4_shim::quit(0);
}

fn run(log: &mut Log) {
	// SAFETY: libkernel returns a static NUL-terminated string.
	let word = unsafe { CStr::from_ptr(sceKernelGetFsSandboxRandomWord()) }.to_string_lossy().into_owned();
	log.say(format!("gpu: sandbox word {word}, flexible memory free {} MB", flex()));
	let dirs = [format!("/{word}/common/lib"), "/system/common/lib".to_string(), "/data/rac-rs".to_string()];

	// The shader compiler first: if Piglet looks for it when it starts, it must already be there.
	let mut shacc = None;
	for d in &dirs {
		let p = format!("{d}/libSceShaccVSH.sprx");
		match load(&p) {
			Ok(h) => {
				log.say(format!("gpu: shader compiler loaded from {p} (handle {h})"));
				shacc = Some(p);
				break;
			}
			Err(e) => log.say(format!("gpu: no shader compiler at {p}: {e}")),
		}
	}

	let mut pig = None;
	for d in &dirs[..2] {
		let p = format!("{d}/libScePigletv2VSH.sprx");
		match load(&p) {
			Ok(h) => {
				log.say(format!("gpu: Piglet loaded from {p} (handle {h})"));
				pig = Some(h);
				break;
			}
			Err(e) => log.say(format!("gpu: Piglet not loaded from {p}: {e}")),
		}
	}
	if pig.is_none() {
		return;
	}
	// The other system libraries: anything that looks like a shader compiler (run-time GLSL needs one; the first run found no
	// libSceShaccVSH.sprx), and the whole list once, for reference.
	for d in [format!("/{word}/common/lib"), "/system/common/lib".into(), "/system/priv/lib".into(), "/system_ex/common_ex/lib".into(), format!("/{word}/priv/lib")] {
		match std::fs::read_dir(&d) {
			Ok(rd) => {
				let mut names: Vec<String> = rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
				names.sort();
				let hits: Vec<&String> = names.iter().filter(|n| ["shac", "shader", "compil", "glsl", "pssl"].iter().any(|k| n.to_lowercase().contains(k))).collect();
				log.say(format!("gpu: {d}: {} files; shader-related: {hits:?}", names.len()));
				log.say(format!("gpu:   {}", names.join(" ")));
			}
			Err(e) => log.say(format!("gpu: {d}: cannot list ({e})")),
		}
	}
	// WebKit's precompiled shaders: without a run-time compiler they are the only PSSL binaries at hand, so dump them all for study.
	let pre = format!("/{word}/common/lib/libScePrecompiledShaders.sprx");
	match load(&pre) {
		Ok(h) => {
			log.say(format!("gpu: precompiled shaders loaded (handle {h})"));
			dump_precompiled(log);
		}
		Err(e) => log.say(format!("gpu: precompiled shaders not loaded: {e}")),
	}
	let gl = Gl::load();
	// SAFETY: from here on, GL / EGL calls with valid arguments on this thread's context.
	unsafe { draw_tests(&gl, log, shacc.is_some()) };
}

/// One entry of libScePrecompiledShaders' table (OpenOrbis piglet sample): name, MD5, size, code; a null name ends it.
#[repr(C)]
struct ShaderBlob {
	ident: *const c_char,
	hash: [u8; 16],
	len: u64,
	code: *const u8,
}

unsafe extern "C" {
	static scePrecompiledShaderEntries: [ShaderBlob; 0];
}

/// Lists WebKit's precompiled shaders in the log and writes each to /data/rac-rs/shaders/ (name with '/' as '_').
fn dump_precompiled(log: &mut Log) {
	let dir = "/data/rac-rs/shaders";
	let _ = std::fs::create_dir_all(dir);
	let mut names = Vec::new();
	// SAFETY: the table is the loaded module's export; it ends with a null name (as the sample walks it). The cap guards a
	// table without one.
	unsafe {
		let mut e = scePrecompiledShaderEntries.as_ptr();
		for _ in 0..4096 {
			if (*e).ident.is_null() {
				break;
			}
			let name = CStr::from_ptr((*e).ident).to_string_lossy().into_owned();
			let len = (*e).len as usize;
			if !(*e).code.is_null() && len < 1 << 22 {
				let bytes = std::slice::from_raw_parts((*e).code, len);
				let _ = std::fs::write(format!("{dir}/{}", name.replace('/', "_")), bytes);
			}
			names.push(format!("{name} ({len})"));
			e = e.add(1);
		}
	}
	log.say(format!("gpu: {} precompiled shaders, written to {dir}: {}", names.len(), names.join(", ")));
}

unsafe fn draw_tests(gl: &Gl, log: &mut Log, have_shacc: bool) {
	unsafe {
		// The OpenOrbis sample's configuration (flat_z's values).
		let mut cfg = PglConfig([0; 0x88]);
		cfg.set32(0x00, 0x88);
		cfg.set32(0x04, 0x8 | 0x80 | 0x60);
		cfg.0[0x08] = 1;
		cfg.set64(0x18, 250 << 20);
		cfg.set64(0x28, 512 << 20);
		cfg.set64(0x30, 170 << 20);
		cfg.set32(0x40, W as u32);
		cfg.set32(0x44, H as u32);
		cfg.set32(0x54, 1 << 20);
		cfg.set32(0x58, 1 << 20);
		cfg.set32(0x5c, 2);
		if !(gl.scePigletSetConfigurationVSH)(&cfg) {
			log.say("gpu: scePigletSetConfigurationVSH failed");
			return;
		}
		log.say(format!("gpu: Piglet configured; flexible memory free {} MB", flex()));
		let dpy = (gl.eglGetDisplay)(0);
		log.say(format!("gpu: eglGetDisplay -> {dpy:?} (error {:#x})", (gl.eglGetError)()));
		let (mut maj, mut min) = (0, 0);
		let r = if dpy.is_null() { 0 } else { (gl.eglInitialize)(dpy, &mut maj, &mut min) };
		log.say(format!("gpu: eglInitialize -> {r} (error {:#x})", (gl.eglGetError)()));
		if r == 0 {
			return;
		}
		(gl.eglBindAPI)(EGL_OPENGL_ES_API);
		(gl.eglSwapInterval)(dpy, 0);
		let attribs = [0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 8, 0x3025, 0, 0x3026, 0, 0x3032, 0, 0x3031, 0, 0x3040, 4, 0x3033, 4, EGL_NONE];
		let mut config = core::ptr::null_mut();
		let mut n = 0;
		if (gl.eglChooseConfig)(dpy, attribs.as_ptr(), &mut config, 1, &mut n) == 0 || n < 1 {
			log.say(format!("gpu: eglChooseConfig failed {:#x} ({n} configs)", (gl.eglGetError)()));
			return;
		}
		let win = PglWindow { id: 0, w: W as u32, h: H as u32, pad: 0 };
		let surf = (gl.eglCreateWindowSurface)(dpy, config, &win, [0x3086, 0x3084, EGL_NONE].as_ptr());
		let ctx = (gl.eglCreateContext)(dpy, config, core::ptr::null_mut(), [0x3098, 2, EGL_NONE].as_ptr());
		if surf.is_null() || ctx.is_null() || (gl.eglMakeCurrent)(dpy, surf, surf, ctx) == 0 {
			log.say(format!("gpu: surface / context failed {:#x}", (gl.eglGetError)()));
			return;
		}
		let s = |e| {
			let p = (gl.glGetString)(e);
			if p.is_null() { String::from("?") } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
		};
		log.say(format!("gpu: EGL {maj}.{min}; GL {} / {} / {} / GLSL {}", s(GL_VERSION), s(GL_VENDOR), s(GL_RENDERER), s(GL_SHADING_LANGUAGE_VERSION)));
		log.say(format!("gpu: extensions: {}", s(GL_EXTENSIONS)));
		let mut b = 0u8;
		(gl.glGetBooleanv)(GL_SHADER_COMPILER, &mut b);
		let (mut nbin, mut maxtex) = (0, 0);
		(gl.glGetIntegerv)(GL_NUM_SHADER_BINARY_FORMATS, &mut nbin);
		(gl.glGetIntegerv)(GL_MAX_TEXTURE_SIZE, &mut maxtex);
		log.say(format!("gpu: GL_SHADER_COMPILER {b}, {nbin} shader binary formats, max texture {maxtex}; flexible memory free {} MB", flex()));

		// Shaders from source.
		let compile = |kind: Enum, src: &str, log: &mut Log| -> Option<u32> {
			let sh = (gl.glCreateShader)(kind);
			let p = src.as_ptr() as *const c_char;
			let len = src.len() as i32;
			let t = Instant::now();
			(gl.glShaderSource)(sh, 1, &p, &len);
			(gl.glCompileShader)(sh);
			let mut ok = 0;
			(gl.glGetShaderiv)(sh, GL_COMPILE_STATUS, &mut ok);
			let mut info = vec![0u8; 2048];
			let mut il = 0;
			(gl.glGetShaderInfoLog)(sh, info.len() as i32, &mut il, info.as_mut_ptr() as *mut c_char);
			let info = String::from_utf8_lossy(&info[..il.clamp(0, 2048) as usize]).into_owned();
			log.say(format!(
				"gpu: compile {} shader: status {ok}, gl error {:#x}, {:.1} ms, log: {}",
				if kind == GL_VERTEX_SHADER { "vertex" } else { "fragment" },
				(gl.glGetError)(),
				t.elapsed().as_secs_f64() * 1000.0,
				info.trim()
			));
			(ok != 0).then_some(sh)
		};
		let vs = compile(GL_VERTEX_SHADER, VS, log);
		let fs = compile(GL_FRAGMENT_SHADER, FS, log);
		let prog = match (vs, fs) {
			(Some(vs), Some(fs)) => {
				let p = (gl.glCreateProgram)();
				(gl.glAttachShader)(p, vs);
				(gl.glAttachShader)(p, fs);
				(gl.glBindAttribLocation)(p, 0, c"a_pos".as_ptr());
				(gl.glBindAttribLocation)(p, 1, c"a_uv".as_ptr());
				(gl.glBindAttribLocation)(p, 2, c"a_col".as_ptr());
				(gl.glLinkProgram)(p);
				let mut ok = 0;
				(gl.glGetProgramiv)(p, GL_LINK_STATUS, &mut ok);
				let mut info = vec![0u8; 2048];
				let mut il = 0;
				(gl.glGetProgramInfoLog)(p, info.len() as i32, &mut il, info.as_mut_ptr() as *mut c_char);
				log.say(format!("gpu: link: status {ok}, log: {}", String::from_utf8_lossy(&info[..il.clamp(0, 2048) as usize]).trim()));
				(ok != 0).then_some(p)
			}
			_ => None,
		};
		let Some(prog) = prog else {
			log.say(format!(
				"gpu: RESULT: no run-time shaders ({}). Showing a colour cycle to prove the swap chain works.",
				if have_shacc { "even with the shader compiler module loaded" } else { "no shader compiler module found" }
			));
			for i in 0..180 {
				let t = i as f32 / 180.0;
				(gl.glClearColor)(t, 0.2, 1.0 - t, 1.0);
				(gl.glClear)(GL_COLOR_BUFFER_BIT);
				(gl.eglSwapBuffers)(dpy, surf);
			}
			return;
		};
		log.say("gpu: RESULT: run-time GLSL ES shaders work.");
		(gl.glUseProgram)(prog);
		(gl.glUniform1i)((gl.glGetUniformLocation)(prog, c"s_tex".as_ptr()), 0);

		// Two textures (checkers in two colours) and a render target, as the GS samples textures and its own buffers.
		let mut tex = [0u32; 3];
		(gl.glGenTextures)(3, tex.as_mut_ptr());
		for (i, &t) in tex.iter().enumerate() {
			let (w, data) = if i < 2 {
				let c = if i == 0 { [255u8, 200, 60, 255] } else { [60, 160, 255, 255] };
				let px: Vec<u8> = (0..256 * 256).flat_map(|k| if ((k % 256) / 32 + (k / 256) / 32) % 2 == 0 { c } else { [30, 30, 30, 255] }).collect();
				(256, Some(px))
			} else {
				(512, None)
			};
			(gl.glBindTexture)(GL_TEXTURE_2D, t);
			(gl.glTexImage2D)(GL_TEXTURE_2D, 0, GL_RGBA as i32, w, w, 0, GL_RGBA, GL_UNSIGNED_BYTE, data.as_ref().map_or(core::ptr::null(), |d| d.as_ptr() as *const c_void));
			(gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
			(gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
			(gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
			(gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
		}
		let mut fbo = 0u32;
		(gl.glGenFramebuffers)(1, &mut fbo);
		(gl.glBindFramebuffer)(GL_FRAMEBUFFER, fbo);
		(gl.glFramebufferTexture2D)(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex[2], 0);
		let st = (gl.glCheckFramebufferStatus)(GL_FRAMEBUFFER);
		log.say(format!("gpu: render-to-texture framebuffer status {st:#x} ({})", if st == GL_FRAMEBUFFER_COMPLETE { "complete" } else { "NOT complete" }));
		(gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
		let mut vbo = 0u32;
		(gl.glGenBuffers)(1, &mut vbo);
		(gl.glBindBuffer)(GL_ARRAY_BUFFER, vbo);
		let stride = core::mem::size_of::<Vtx>() as i32;
		(gl.glVertexAttribPointer)(0, 2, GL_FLOAT, 0, stride, core::ptr::null());
		(gl.glVertexAttribPointer)(1, 2, GL_FLOAT, 0, stride, 8 as *const c_void);
		(gl.glVertexAttribPointer)(2, 4, GL_UNSIGNED_BYTE, 1, stride, 16 as *const c_void);
		for a in 0..3 {
			(gl.glEnableVertexAttribArray)(a);
		}
		log.say(format!("gpu: set up, gl error {:#x}, flexible memory free {} MB", (gl.glGetError)(), flex()));

		// Benchmarks: (triangles a frame, triangles per draw call). R&C draws a few thousand GS primitives a field with frequent
		// texture / blend changes; 1 call is the best case, small batches the GS-like case.
		let cases: [(usize, usize); 5] = [(2_000, 2_000), (20_000, 20_000), (20_000, 20), (60_000, 30), (10_000, 5)];
		let mut seed = 0x1234_5678u32;
		let mut rnd = move || {
			seed ^= seed << 13;
			seed ^= seed >> 17;
			seed ^= seed << 5;
			seed as f32 / u32::MAX as f32
		};
		for (ntri, per) in cases {
			// Six vertices of the full-screen quad (the render target's picture), then the small triangles.
			let mut v = Vec::with_capacity(6 + ntri * 3);
			let q = |x: f32, y: f32| Vtx { x, y, u: (x + 1.0) / 2.0, v: (y + 1.0) / 2.0, rgba: [0x80; 4] };
			v.extend([q(-1.0, -1.0), q(1.0, -1.0), q(1.0, 1.0), q(-1.0, -1.0), q(1.0, 1.0), q(-1.0, 1.0)]);
			for _ in 0..ntri {
				let (cx, cy, r) = (rnd() * 2.0 - 1.0, rnd() * 2.0 - 1.0, 0.02 + rnd() * 0.06);
				let col = [(rnd() * 128.0) as u8 + 64, (rnd() * 128.0) as u8 + 64, (rnd() * 128.0) as u8 + 64, 0x60];
				for k in 0..3 {
					let a = k as f32 * 2.094 + rnd();
					v.push(Vtx { x: cx + r * a.cos(), y: cy + r * a.sin(), u: rnd(), v: rnd(), rgba: col });
				}
			}
			let frames = 120;
			let (mut cpu, mut total) = (0f64, 0f64);
			for f in 0..frames {
				let t = Instant::now();
				// Upload every frame, as the game's vertices change every field.
				(gl.glBufferData)(GL_ARRAY_BUFFER, (v.len() * stride as usize) as isize, v.as_ptr() as *const c_void, GL_STREAM_DRAW);
				// Pass 1: 200 triangles into the 512x512 render target.
				(gl.glBindFramebuffer)(GL_FRAMEBUFFER, fbo);
				(gl.glViewport)(0, 0, 512, 512);
				(gl.glClearColor)(0.1, 0.0, 0.2, 1.0);
				(gl.glClear)(GL_COLOR_BUFFER_BIT);
				(gl.glActiveTexture)(GL_TEXTURE0);
				(gl.glBindTexture)(GL_TEXTURE_2D, tex[0]);
				(gl.glDrawArrays)(GL_TRIANGLES, 6, 600.min(ntri as i32 * 3));
				// Pass 2: the screen: the render target as background, then the triangles in batches.
				(gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
				(gl.glViewport)(0, 0, W, H);
				(gl.glDisable)(GL_BLEND);
				(gl.glBindTexture)(GL_TEXTURE_2D, tex[2]);
				(gl.glDrawArrays)(GL_TRIANGLES, 0, 6);
				(gl.glEnable)(GL_BLEND);
				let mut first = 6;
				let mut call = 0usize;
				while first < v.len() {
					let cnt = (per * 3).min(v.len() - first);
					// A state change per call: texture and blend mode alternate (the GS's ALPHA and TEX0 registers).
					(gl.glBindTexture)(GL_TEXTURE_2D, tex[call & 1]);
					if call & 2 == 0 {
						(gl.glBlendFunc)(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
					} else {
						(gl.glBlendFunc)(GL_SRC_ALPHA, GL_ONE);
					}
					(gl.glDrawArrays)(GL_TRIANGLES, first as i32, cnt as i32);
					first += cnt;
					call += 1;
				}
				let c = t.elapsed().as_secs_f64();
				(gl.glFinish)();
				let tt = t.elapsed().as_secs_f64();
				(gl.eglSwapBuffers)(dpy, surf);
				if f >= 10 {
					cpu += c;
					total += tt;
				}
			}
			let n = (frames - 10) as f64;
			log.say(format!(
				"gpu: {ntri} triangles in {} calls a frame: submit {:.2} ms, submit + GPU finish {:.2} ms a frame (gl error {:#x})",
				ntri.div_ceil(per) + 2,
				cpu * 1000.0 / n,
				total * 1000.0 / n,
				(gl.glGetError)()
			));
		}
	}
}
