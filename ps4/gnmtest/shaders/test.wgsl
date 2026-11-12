// PS4 port, GNM test 2 (ps4/gnmtest): WGSL compiled for the PS4's GPU by Mesa radv (WSL, ps4/tools/shaders.sh), run through
// the GNM layer. Two pipelines exercise radv's resource ABI the way rac-rs's renderer will:
//   bg_vs / bg_fs: a screen-covering triangle from the vertex index (no vertex buffers) that samples a texture through
//                  descriptor set 0 (uniform buffer, texture, sampler) and tints it with the uniform.
//   cube_vs / cube_fs: a spinning cube from two vertex buffers (position, colour), with a depth test, its matrix in set 0.

struct U {
	mvp: mat4x4<f32>,
	tint: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;

struct BgOut {
	@builtin(position) pos: vec4<f32>,
	@location(0) uv: vec2<f32>,
}

@vertex fn bg_vs(@builtin(vertex_index) i: u32) -> BgOut {
	let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
	var o: BgOut;
	o.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
	o.uv = vec2<f32>(p.x, 1.0 - p.y) * vec2<f32>(16.0 / 9.0, 1.0) * 4.0;
	return o;
}

@fragment fn bg_fs(v: BgOut) -> @location(0) vec4<f32> {
	return textureSample(tex, smp, v.uv) * u.tint;
}

struct CubeOut {
	@builtin(position) pos: vec4<f32>,
	@location(0) col: vec3<f32>,
}

@vertex fn cube_vs(@location(0) p: vec3<f32>, @location(1) c: vec3<f32>) -> CubeOut {
	var o: CubeOut;
	o.pos = u.mvp * vec4<f32>(p, 1.0);
	o.col = c;
	return o;
}

@fragment fn cube_fs(v: CubeOut) -> @location(0) vec4<f32> {
	return vec4<f32>(v.col, 1.0);
}
