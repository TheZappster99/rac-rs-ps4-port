# PS4 port: the GPU renderer (design, 2026-11-06)

Hand-off and status of the whole PS4 port: `PS4_PORT_PLAN.md`. This file is the design for the playable picture (step 2,
part 4 onward). Nothing here is built yet.

## Where we start

- **The core runs at full speed** in a level (EE thread 16.2 ms per 16.7 ms field, one core). The PS4 gives an app 6-7
  Jaguar cores, each about 6-7x slower than this PC's.
- **The game's own picture works on the console** through GS-full (`ps4/probe`, `gsf.rs`). It is byte-identical to the
  PC's, but drawn by the CPU: ~3 pictures a second. VIF1+VU1 takes 62-65 ms a field and the software GS 260-325 ms (title
  screen, 3 raster threads).
- **We can drive the GPU** (`ps4/gnm`): own shaders (LLVM IR -> `llc -mcpu=gfx701`), own PM4 command buffers, end-of-pipe
  labels, video out flips. The test frame took under 1 ms of GPU time.
- **There is no shader compiler on the console.** Every shader must be compiled on the PC and shipped as GCN machine code.
- **The game draws a lot through the GS**, per field in a level (`ps4/probe/src/census.rs`, PC run, fields 1300-2600,
  `PROBE_CENSUS=FROM:TO`):

  | What | Per field |
  |---|---|
  | Primitives | 53,600 (99% triangle strips; worst field 133,000) |
  | Draw state changes | 652 (worst 3,558) |
  | Texture uploads | 79, 53,400 qwords (~850 KB) |
  | CLUT loads | 1,705 |
  | Prims that sample the frame they draw into | 726 (screen effects; also 376 with a frame mask) |

  - **Textures:** 97% are 8-bit paletted (PSMT8, 0x13), the rest 32- and 24-bit colour.
  - **Blending:** almost all `(Cs-Cd)*As+Cd`, plus some `(Cs-0)*As+Cd` and `(Cs-Cd)*FIX+Cd`. All of them fit the GPU's
    fixed-function blender, apart from the PS2's alpha range above 1.0 (As up to 255/128).
  - **Alpha test:** mostly GEQUAL with AFAIL = RGB_ONLY (failing pixels still write colour, but not alpha or Z).
  - **Depth:** GEQUAL, Z24, depth writes on.
  - **Pixel work:** fog and Gouraud shading on most draws, bilinear filtering, TFX modulate.
  - **Buffers:** one 512x448 32-bit scene buffer at block 0xe00 takes 99.8% of prims.
  - **Draw states:** 2,265 distinct, but they collapse to a few shader variants plus dynamic state.

## The fork: which picture?

### A. GPU back end for GS-full ("hardware GS", like PCSX2's hardware renderer)

Keep the existing chain: EE -> VIF1/VU1 (`gsfull-vu1` thread, CPU) -> GIF stream. Then swap the software GS for a GPU one:

- **PS2 local memory (4 MB)** lives in GPU-visible memory. The GPU applies uploads and local copies in stream order, so no
  CPU raster model is needed.
- **Textures are sampled straight from the swizzled local memory** in the pixel shader. That covers PSMT8 with the CLUT,
  and CT32/CT24. Bilinear filtering is done by hand (4 index fetches, 4 CLUT reads). There is no texture cache to keep
  coherent.
  - CLUTs are snapshotted per load into a ring (1 KB each, ~1.7 MB a field), because the game reloads them 1,705 times a
    field.
- **Render targets are GPU images**, at 1x or an upscale (2x/3x for 1080p would be a free bonus). They are written back
  into local memory only when something reads them as a texture (the 726 feedback prims: copy-before-draw), and by uploads
  or CRTC reads.
- **Draw state:**
  - Fixed-function blending.
  - AFAIL RGB_ONLY as two passes (pass then fail with Z and alpha writes off), as PCSX2 does.
  - Z24 GEQUAL on D24 or D32F.
  - Fog, TFX, clamp and region-repeat in the shader.
  - Positions are already screen space (w = 1), so linear interpolation of S, T, Q, colour and fog matches the PS2.
- **What A gives:** the PS2's exact look, the same front end as the reference picture, and a fairly small GPU side, since
  R&C uses a narrow slice of the GS.
- **What A costs:**
  - The VU1 stage stays on the CPU. It took 62 ms a field on the PS4 at the title screen; in a level it ranged 0.8-13 ms on
    the PC, so roughly 5-90 ms on the PS4. That is the biggest risk.
  - Mods that draw through rac-rs's renderer (Ragdoll Ratchet's body) would not show, because GS-full only shows what the
    game itself draws.
  - No PC graphics options.

### B. Port rac-rs's own renderer (what the PC game shows)

The PC game draws the 3D world itself from the game's memory (the "mirror", `rac-rs/src/viewer*`, 28k lines, wgpu: 32
render and 12 compute pipeline sites, 78 fragment and 29 compute entry points). It uses the GS capture only for the 2D layer.
Port it by giving wgpu a GNM back end:

- **A `wgpu-hal` back end for GNM.** Devices, buffers, textures and samplers become PS4 resource descriptors (V#/T#/S#).
  Bind groups become user SGPRs plus descriptor tables, render passes become CB/DB registers, pipelines become PM4 state,
  and fences become EOP labels. The viewer, and the GS capture's GPU path (`gscap/draw/gpu.wgsl`), would then run unchanged.
- **Shaders compiled offline:** WGSL -> naga -> SPIR-V -> **Mesa radv's compiler for GFX7 with its null device** (no AMD
  GPU needed; runs in WSL, which this PC has: Kali, `mesa-vulkan-drivers` not installed yet). That gives GCN code plus
  register and user-SGPR layout for each pipeline.
  - A PC run records every pipeline the viewer creates (module hash, entry point, layout, constants). The PS4 build ships
    that cache and only looks pipelines up at run time.
  - Fallback if radv cannot be used: a naga-IR -> LLVM IR back end of our own, for the subset the viewer uses, fed to `llc`.
- **What B gives:** everything the PC game has, including every mod's visuals (Ragdoll), and a choice of PC graphics
  options. No VU1 emulation is needed for the world.
- **What B costs:**
  - By far the most work: the wgpu-hal back end, the shader tooling, and texture formats and tiling.
  - The mirror's CPU cost per frame on the PS4 is unknown.
  - Ray tracing stays off.

### What both need first: a GNM layer (`ps4/gnm` grows into a crate)

- Direct-memory allocator (garlic for GPU data, onion for CPU readback).
- PM4 builder: done in the triangle test, needs to become a module.
- Frame pacing (EOP labels, flips, `sceGnmSubmitDone`), resource descriptors, render targets with depth, and an upload ring.

This is the first milestone either way.

## Recommendation

**B is where we should end up.** It is the game you play on the PC, it keeps Ragdoll Ratchet, and it needs no VU1 on the
CPU. Its size makes it worth three cheap measurements before committing. Each one also decides whether A stays a fallback:

1. **VU1 on the console, in a level:** the probe with GS-full's raster stage switched off (fields dropped after VIF1+VU1),
   so the game runs at speed and the log shows the VU1 stage's real cost. If it fits in about 16 ms, A is a fallback; if
   not, A needs VU1 work first.
2. **The mirror's CPU cost:** the PC game with `RAC_PERF_LOG=1`, walking in a level; worker sections times 6.5 gives the
   PS4 estimate.
3. **Offline shader compile:** in WSL, install Mesa's Vulkan drivers and compile one viewer shader for GFX7 with radv's null
   device; check the code and register data come out. Needs your OK to install packages in Kali.

### Measurement results (2026-11-06)

1. **VU1 stage (A's risk):** on the PC, probe in "VU1 only" mode (`PROBE_GS=vu` / `gs_mode.txt`, the default of the probe
   PKG built today), in the level: VIF1+VU1 averages **3.8-5.4 ms a field, worst 10.7-13.4 ms**. Times 6.5 gives **~25-35 ms
   on the PS4, worst ~85 ms**, twice the field budget on one core. **Console run (2026-11-07):** in level 7 while walking
   (fields ~1900-3000), VIF1+VU1 is **26-32 ms a field on average, worst 35-48 ms**. Right after the level loads (fields
   ~1335-1735) it is 64-73 ms. The game thread waits 8-12 ms a field for it, so the level ran at 55-64% speed. The menus
   and title cost 60 ms a field. Conclusion: A only works if the VU1 stage is split across at least 2-3 cores, which
   strengthens the case for B. Also seen: fields ~630-1170 (the level loading in) took 36-38 ms of EE time while VU1 took only 4 ms. That is the
   "post-load slow stretch" from the port plan, and it is EE work, not GS work.
2. **rac-rs's renderer (B's CPU cost):** the PC game run unattended into level 7 (`RAC_RETAIL_PAD` + `RAC_RETAIL_KEYTAP` +
   `RAC_RETAIL_SMOKE=2700`, scratch player dir `build-ps4/pcrun`, `RAC_PERF_LOG=1 RAC_FRAME_PROFILE=1`). Steady walking:
   - **Render thread:** 2.7-3.5 ms of CPU a display frame, up to 5 ms in busy spots. That is mostly cull 0.9-1.8, upload
     0.6-1.4, encode 0.3-0.8, hero 0.35, props 0.35, plus 0.3-0.4 of ray tracing the PS4 drops.
   - **Worker, after `Machine::frame`:** snap 0.3-0.4 ms. The effects layer's VU1 model (`efx_vu`, its own thread) 1.2-1.7 ms.
   - **PS4 estimate (x6.5):**
     - Render thread **~17-30 ms** per frame at one frame a field. That needs work (cull and upload split across cores;
       the GNM back end has no Vulkan driver overhead).
     - Worker: EE 16.2 + snap ~2.5 ms on one thread, slightly over budget. Move EE work such as `skin_ff` to another core,
       as the port plan already lists.
     - efx VU1 ~8-11 ms on its own thread.
   - It fits the 6 cores if the render thread is split. Unlike A's VU1 stage, this is ordinary Rust code we can parallelise.
3. **Offline shader compile (B's tooling): works.**
   - **Setup:** WSL Kali with `mesa-vulkan-drivers` and `mesa-drm-shim` (Mesa 26.1.6). radv runs on a fake GFX7 GPU
     (`LD_PRELOAD=libamdgpu_noop_drm_shim.so AMDGPU_GPU_ID=bonaire`, "AMD Radeon R7 200 Series (RADV BONAIRE)", Vulkan 1.3).
     `RADV_FORCE_FAMILY` is gone from Mesa; the drm-shim replaces it. Its GFX7 ids are only pitcairn (GFX6) and bonaire.
   - **Tools:** `ps4/tools/wgsl2spv` (Rust, naga =30.0.1: WGSL -> SPIR-V per entry point, unused code compacted away, plus
     a reflection .txt) and `ps4/tools/vkc/vkc.c` (builds the Vulkan pipeline from it and prints radv's statistics; run
     through `build-ps4/wsl/vkc.sh`). WSL calls from Git Bash need `MSYS_NO_PATHCONV=1`.
   - **Coverage:** of the 25 modules the PC game dumped (`RAC_WGSL_DIR`), 227 of 232 entry points convert. The 5 failures
     are ray-tracing shaders with pipeline overrides.
   - **Compiled with no spills:** level vs + fs_ocean (VS 328 B, FS 292 B), level sky, post fs_comp (6 KB), post fs_ao,
     vegetation blades, tie_rewrite and water cs_wave (compute).
   - **Heavier cases:** water fs_water is 142 KB of GCN with 472 spilled SGPRs. sky_model fs_reduce needs 72 KB of scratch.
   - The level shaders dumped from this PC have ray-traced shadows compiled in (they bind an acceleration structure). The
     PS4 build needs the viewer's non-RT variants.
   - radv offers **`VK_KHR_pipeline_binary`** here: the way to pull out the GCN code plus register / user-SGPR data for a
     PS4 shader cache.

### The shader cache: radv binaries -> .rsh (done 2026-11-06, overnight)

- `vkc` with `VKC_DUMP=DIR` exports each compiled stage as a `VK_KHR_pipeline_binary` blob (`build-ps4/wsl/dump.sh "VS FS" "CS"`).
- **The blob is radv's `radv_shader_binary_legacy`:** `ac_shader_config`, `radv_shader_info`, `total_size`, then the sizes,
  statistics and the GCN code (`exec_size` bytes; `code_size` adds prefetch padding).
  - The exact Mesa 26.1.6 structs come from Debian's source browser (`sources.debian.org/data/main/m/mesa/26.1.6-1/...`;
    gitlab blocks scripts). They are copied into `ps4/tools/radvbin/layout.c`, which GCC turns into `src/layout.rs` (the
    offsets) in WSL: `build-ps4/wsl/layout.sh`.
  - **If Kali updates Mesa, rerun `layout.sh` against the new headers.** radvbin refuses blobs whose sizes or cross-checks
    do not fit.
- `ps4/tools/radvbin` writes one `.rsh` per stage (format in its main.rs). It checks: total size; `s_endpgm` present; the PS
  input enables equal in config and info; `num_inputs` equals popcount(`input_mask`); the CS block equals `workgroup_size`;
  user SGPRs inside RSRC2.USER_SGPR.
  - All 14 test stages pass, and their SGPR / VGPR counts equal radv's statistics. The stages: level vs/fs_ocean, post
    vs_full/fs_ao, sky_model fs_reduce, veg blade vs/fs, water vs_grid/fs_water/cs_wave, tie_rewrite.
- **In 26.1.6 the precomputed registers (`radv_shader_regs`) are no longer serialized.** The GNM layer computes them from
  config + info, as radv does when it loads a binary:
  - VS: `spi_vs_out_config` and pos format from `param_exports`, the clip / cull masks and point size.
  - PS: `spi_ps_in_control` from `num_inputs`; `spi_ps_input_cntl_n` from the VS parameter map; `db_shader_control` /
    `z_format`.
- **radv's GFX7 resource ABI, as the blobs show it:**
  - User SGPRs 0-1 = scratch-ring table pointer, always present.
  - Each descriptor set = one user SGPR holding a **32-bit pointer whose high half is 0** (radv's `address32_hi` is 0 below
    GFX9). VS: vertex-buffer descriptor table pointer (1 SGPR) and base vertex / start instance. Push constants likewise.
  - **So descriptor sets, vertex-buffer tables and push constants must sit below 4 GB of virtual address space on the
    PS4.** Our direct memory mapped at 0x2_0040_0000. The next console test tries a fixed low mapping.
  - VS outputs reach PS inputs in order (VAR0->param 0, ...). The colour export format follows the attachment formats at
    compile time (FP16_ABGR = 4 for the RGBA16F the tests used), so compile with the PS4's real target formats.

**Reading:** B's two unknowns came out favourable (shaders compile; CPU cost is high but parallelisable). A's VU1 stage
looks over budget unless the console disagrees. Pending: the console's VU1 number.

Then the milestones for B:

1. GNM layer crate (allocator, PM4, frames, descriptors, render target + depth). Test: the triangle with a texture and depth.
2. Shader pipeline: WGSL -> SPIR-V -> radv (WSL) -> cache file. Test: a viewer shader drawn on the console.
3. wgpu-hal GNM back end for what the viewer uses. Test: `view --shot` scenes rendered on the console from a saved snapshot.
4. The real game: core + mirror + renderer + GS capture overlay, then pad input and sound (step 3 of the port plan).

## wgpu on GNM: the back-end design (option B, written 2026-11-06 overnight)

### How it plugs in without forking wgpu

- wgpu 30 has a `noop` hal back end (feature `noop`, `wgt::NoopBackendOptions { enable: true }`). wgpu-core drives it
  like any other: adapters, devices, queues, and surfaces too, because `Instance::create_surface` asks every enabled
  back end's hal `Instance`.
- wgpu-hal is already vendored and patched (`rac-rs/vendor/wgpu-hal`, the `[patch.crates-io]` at the workspace root).
- **For the PS4 target only, the vendored hal's `noop` module becomes the GNM back end**
  (`#[cfg(target_os = "freebsd")] #[path = "gnm/mod.rs"] pub mod noop;`). It builds on `ps4/gnmlib`.
- wgpu, wgpu-core and rac-rs's renderer code stay as they are. The PS4 build only asks wgpu for the noop back end.

### hal object -> GNM

| wgpu-hal | GNM back end |
|---|---|
| Instance / Adapter | One adapter "PS4 GPU (GNM)". Its features and limits are what the GFX7 radv device reported, cut to what the viewer asks for. |
| Device memory, Buffer | Direct memory (garlic, CPU+GPU RW); a buffer is an address range. `map_buffer` returns the pointer (unified memory); flush and invalidate are an `sfence` and nothing. |
| Texture / TextureView | Colour targets and sampled textures: linear aligned (tile index 8) at first. Depth: 1D thin (tile index 5). Mips and arrays: per-level offsets with the linear-aligned pitch rule. The view keeps a T# (and, for targets, the CB / DB register set). Tiled colour (2D thin) later, for speed. |
| Sampler | An S# (`desc::sampler`, extended with LOD, anisotropy, compare). |
| BindGroupLayout / BindGroup | **radv's descriptor set layout, exactly.** Binding offsets and sizes come from the shader cache (see below). A bind group is a block in the **below-4 GB arena** with V# / T# / S# written at those offsets. |
| PipelineLayout | The bind group layouts plus the immediates (push constant) size. |
| ShaderModule | Keeps the `naga::Module`. There is no compiler on the console. |
| RenderPipeline / ComputePipeline | Looked up in the **shipped shader cache** by a key (below). It holds the .rsh stages (code uploaded once), the register words, the user-SGPR map, and the fixed state: blend (CB_BLEND0..7_CONTROL), depth / stencil (DB_DEPTH_CONTROL...), raster (PA_SU_SC_MODE_CNTL), primitive type. A missing key is logged and appended to `/data/rac-rs/missing_pipelines.txt`, and draws with it are skipped. |
| CommandEncoder | A chain of PM4 buffers in direct memory (`pm4::Cmd`). |
| `begin_render_pass` | Targets (`target::*`, extended to 8 colour targets and the formats the viewer uses). Clears use **CP DMA fills** (DMA_DATA with a constant), which work for linear colour and for tiled depth alike because the value is uniform. |
| `set_bind_group` | User-SGPR writes of the bind group's 32-bit address for each stage of the current pipeline (`Shader::set_sgpr`). Dynamic offsets: radv 26.1 passes them through a small descriptor block (`AC_UD_DYNAMIC_DESCRIPTORS*`); patched copies in the low arena per draw. |
| `set_immediates` | radv's push constants: inline SGPRs for the dwords in `inline_push_constant_mask`, plus a low-arena copy whose address goes in `AC_UD_PUSH_CONSTANTS`. |
| `set_vertex_buffer` / `set_index_buffer` | Vertex buffer table (V# per binding, stride from the pipeline) in the low arena, pointer in `AC_UD_VS_VERTEX_BUFFERS`. Index buffer: INDEX_BASE / INDEX_TYPE and the driver's `sceGnmDrawIndex`. |
| `draw*` | `sceGnmDrawIndexAuto` / `sceGnmDrawIndex`, base vertex / first instance in `AC_UD_VS_BASE_VERTEX_START_INSTANCE`, NUM_INSTANCES. Indirect draws: DRAW_INDIRECT packets (later). |
| `dispatch_workgroups` | The compute shader registers (sceGnmSetCsShader or direct COMPUTE_* writes), then a DISPATCH_DIRECT packet. |
| `copy_buffer_to_buffer`, buffer <-> linear texture | CP DMA (DMA_DATA), row by row for textures. Tiled textures later get a precompiled copy compute shader. |
| `transition_*` (barriers) | Cache actions: CB / DB flush and invalidate, TC / K$ invalidate (SURFACE_SYNC or EVENT_WRITE with the right event). The first version may be conservative: full flush between passes. |
| Fence / Queue::submit | EOP label writes (the counter value), `sceGnmSubmitCommandBuffers` + `sceGnmSubmitDone`. `Device::wait` polls the label. |
| Surface / present | Three textures registered with video out (A8R8G8B8 sRGB or linear); acquire hands out the next one; present waits for its EOP label, then `sceVideoOutSubmitFlip`. |
| Queries, timestamps, ray tracing, mesh shaders | Not offered (the viewer runs without them; ray tracing is PC only). |

### The shader cache key and the capture

- **One key function shared by both sides:** `gnm_key.rs` in the vendored hal, compiled on the PC and on the PS4. It hashes
  what decides the compiled code and its ABI:
  - each stage's naga module, written back out as WGSL by naga's own writer (deterministic for the same naga version and
    input), plus entry point and override constants;
  - the bind group layouts (entries and types);
  - the vertex buffer layouts;
  - the colour target formats and blend (they pick the export format);
  - the depth format and primitive topology.
- **Capture (PC):** the vendored hal's Vulkan back end, with `RAC_PS4_CAPTURE=DIR`, writes one record per pipeline the
  game creates. A record holds:
  - the key, and the SPIR-V the Vulkan hal produced (with its own binding map);
  - the Vulkan set layouts and the wgpu (group, binding) -> (set, binding) map;
  - vertex input, formats and state.
  - A run through every level and menu with each graphics option fills the capture. That is a few runs with the
    unattended pad scripts used for measurement 2.
- **Compile (WSL):** `vkc` replays each record on radv's fake GFX7 device and exports the binaries (`VK_KHR_pipeline_binary`).
  It also asks radv for each set layout's binding offsets: `VK_EXT_descriptor_buffer`,
  `vkGetDescriptorSetLayoutBindingOffsetEXT` and `vkGetDescriptorSetLayoutSizeEXT`. **Checked 2026-11-06** with
  `ps4/tools/vkc/layoutq.c` (`build-ps4/wsl/layoutq.sh`): for {uniform, texture, sampler} radv reports 64 / 0 / 80 (set
  size 96), exactly the offsets in the NIR of shaders compiled with an ordinary set. The ordering follows no simple rule
  ({texture, sampler, uniform, storage, texture} -> 64, 0, 16, 32, 128), so always ask radv. `radvbin` packs everything
  into one cache file, key -> stages + offsets + binding map, shipped in the PKG.
- **Missing keys on the console** go to `missing_pipelines.txt`. Feeding that back to the PC capture closes the gap.

### What else the PS4 build of rac-rs needs (outside the renderer)

- The app loop without winit (`viewer/app.rs`, `handler.rs`): a PS4 main loop that drives the same frame function, takes
  pad input (scePad) and presents through the GNM surface.
- Audio through sceAudioOut, and saves and settings in `/data/rac-rs`.
- Platform-only dependencies cfg-gated: Windows APIs, ReShade, the DX12 path, gilrs.
- The probe already proves the core and std side.

### Work order and size

1. Done tonight: gnmlib, the shader-cache tools, GNM test 2.
2. GNM test 2 on the console. It decides the below-4 GB arena; if the console refuses low mappings, the fallback is to patch
   the high half: radv materialises it as a literal 0, findable through `pack_64_2x32_split` in the final NIR, so the
   compiled code could be post-processed.
3. Capture + compile + cache file, checked on the PC by replaying captured pipelines and comparing reflection.
   **Done 2026-11-07:**
   - **Key:** `rac-rs/vendor/wgpu-hal/src/ps4key.rs` (FNV-1a 64 over the `Debug` text of the naga module, entry point,
     constants, bind group layouts, vertex buffers and fixed state; back-end independent, for the GNM back end too).
   - **Capture:** `vulkan/ps4cap.rs` plus five hooks in `vulkan/device.rs` (stage compile, bind group layout, pipeline
     layout, render and compute pipeline). `RAC_PS4_CAPTURE=DIR` writes `<key>.rec` and the stages' SPIR-V.
   - **Compile and pack:** `ps4/tools/vkc/vkrec.c` (WSL) replays every record on radv GFX7 and exports binaries plus
     `<key>.sets` (radv's binding offsets through descriptor-buffer twins; dynamic buffers listed by index). `radvbin` turns
     them into .rsh, and `ps4/tools/shcache` checks every pipeline and packs `build-ps4/ps4shaders.bin`
     (`rac_ps4_gnm::cache`). One command: `sh ps4/tools/shcache.sh`.
   - **First capture** (title screen to level 7 walking, the three PS4 mods on, ray tracing off): 141 pipelines (113 render,
     28 compute). All compile on GFX7, and all pass the checks: stage order, sets used, every binding laid out, every
     fragment input written by the VS. The cache is 1.2 MB.
   - **Deterministic:** a second identical run gives byte-identical records and SPIR-V (the same 141 keys).
   - **Findings for the back end:**
     - Four shaders spill (water shade, k_redo, k_serial, k_raster), and the sky model needs 72 KB of scratch per wave, so
       the scratch ring (user SGPRs 0-1) must be real for those.
     - The PC run used 4x MSAA and a D32S8 depth target, and the records keep the PC's formats. Before shipping, capture
       with the PS4's own graphics settings (no MSAA, its surface format).
     - Coverage is one walk through one level. Missing keys on the console go to `missing_pipelines.txt`, as designed.
4. The hal back end, in order: buffers / textures / samplers / bind groups -> render pipelines -> render passes and draws ->
   compute -> copies and barriers -> surface. Test: rac-rs `view --shot` scenes from a snapshot, on the console.
   **First version written 2026-11-07:** `rac-rs/vendor/wgpu-hal/src/gnm/` (feature `ps4-gnm`, which puts it in the noop
   slot).
   - **Files:**
     - `mod.rs`: device, queue, surface, resources;
     - `command.rs`: pipelines and the encoder;
     - `pipe.rs`: cache lookup and set layouts;
     - `format.rs`, `mem.rs` (main heap plus the below-4 GB heap);
     - `plat.rs`: the PS4 calls, or a **fake GPU** off the console that records but executes nothing.
   - **Simplifications for now:**
     - Linear textures, and views see one mip level.
     - Barriers drain the GPU (EOP, WAIT_REG_MEM, ACQUIRE_MEM), and only when work was recorded since the last one.
     - Clears and copies go through CP DMA_DATA (direct memory); clear colours of 8 or 16-byte texels need a repeating
       4-byte pattern.
     - No MSAA, indirect draws, queries or scratch yet.
     - A missing pipeline is logged and listed in `missing_pipelines.txt`, and its draws are skipped.
   - **Test program:** `ps4/wgputest`, one wgpu program run three ways:
     - **PC Vulkan:** real pixels, and the capture.
     - **PC fake GNM:** the cache check. All 4 pipelines were found, so the keys match end to end.
     - **PS4:** PKG `IV0000-RACR00006_00-RACRSWGPUTEST000`, log `/data/rac-rs/wgputest.log`.
   - **Vulkan reference pixels:** corner `ff7b89cb` (blue background), centre `ffdf8282` (the cube's red face) on frame 0.
5. The rac-rs PS4 app loop, input, audio, then the real game (core + mirror + renderer).
   **First part done 2026-11-07: the real game on the fake GNM GPU (PC).** `ps4/app` (crate `rac-ps4-app`, its own workspace,
   Cargo.lock copied from the game's) is the game exe's `main` linked against the vendored hal with `ps4-gnm`. No game code
   changed: wgpu's own `WGPU_BACKEND=noop WGPU_NOOP_BACKEND=1` puts the window on the GNM back end.
   - **Build:** the root build line with `--manifest-path ps4/app/Cargo.toml --target-dir build-ps4/target` (10 min the first
     time, 2.5 min after a hal change).
   - **Run:** player dir `build-ps4/fakerun` (copies of `caprun`'s card, mods, options), `RAC_PS4_SHADERS=build-ps4/ps4shaders.bin`,
     `RAC_PS4_MISSING=<dir>/missing_pipelines.txt`, `RAC_RETAIL_SMOKE=3000`,
     `RAC_RETAIL_PAD=220-226:800:80,80;280-286:4000:80,80;320-326:40:80,80;420-426:40:80,80;1300-2900:0:80,0`, `RAC_MUTE=1`,
     plus the usual `ORACLE_*`, then `view <iso> 0 --retail-cold --modern-cam`.
   - **Result:** title, Load Game, level 7, 3000 fields walking, **every pipeline found in the cache** (98 render pipelines plus
     compute, the same keys as a Vulkan run of the same exe). Peak GPU memory **960 MB** of the 2 GB main heap (708 MB is
     already used before the first field: tiegpu, starfield and friends).
   - **Fixes it needed in the back end:**
     - The surface offers `COPY_SRC` (the game's swapchain asks for it).
     - The fake GPU's heap is 2 GB like the console's (512 MB ran out at the level load), and it rebuilds the surface when the
       PC window changes size (the console keeps 1920x1080).
     - **`SHADER_EARLY_DEPTH_TEST` is offered.** `viewer/volfog/soft.rs` writes `@early_depth_test(force)` into the shared level
       shader when the adapter has it, and the capture adapter did, so without it 63 pipelines had other keys. Rule: **the GNM
       adapter must offer every feature the game branches its WGSL on that the capture adapter offered** (the game's list:
       grep `Features::` in rac-rs/src; the others it checks are timestamps, BC textures, pipeline cache, ray query and binding
       arrays, which do not change the cached shaders today).
     - Heap-full messages, a 64 MB-step high-water line and the "not supported" notes go to stderr too (rac-rs installs no
       logger for wgpu-hal, so `log` calls were silent).
   - **DEV: `RAC_PS4_KEYDUMP=DIR`** (ps4key.rs, both back ends; on Vulkan only while `RAC_PS4_CAPTURE` is set) writes the fixed
     state of each render key (`<key>.txt`) and each stage's naga module (`stage_<key>_<entry>.txt`). Diffing two runs' dumps
     is how the early-depth difference was found.
   - **What the back end still drops in this run** (its "not supported" notes, the next work):
     - 4x MSAA drawn single-sampled. Better to run the PS4 build with MSAA off and re-capture with the PS4's settings.
     - Texture views see only their base mip (no mipmaps).
     - **Indirect draws are skipped.** Find which passes use them; they may hide part of the scene.
     - A clear of an Rgba16Float target to (0, 0, 0, 1) is not a 4-byte pattern and clears to zero.
   **Second part (2026-11-07, PC; console check pending): those gaps filled.** The fake-GPU game run (same recipe) now
   reaches level 7 and 3000 fields with every pipeline found and only the MSAA notes left.
   - **Indirect draws** (the bindless tie / shrub passes, the mirror, sun shadows): SET_BASE to the argument buffer, then
     one DRAW_INDIRECT / DRAW_INDEX_INDIRECT per draw (INDEX_BASE + INDEX_BUFFER_SIZE first), naming the VS user-data
     registers of AC_UD_VS_BASE_VERTEX_START_INSTANCE so the CP writes base vertex and first instance there, as radv does.
     One packet per draw rather than the *_MULTI forms, which GNM does not expose. Indirect dispatch: DISPATCH_INDIRECT.
   - **Grid size on GFX7 is a pointer:** radv before GFX10.3 passes AC_UD_CS_GRID_SIZE as one SGPR holding a 32-bit
     pointer to three dwords. Direct dispatches now upload the counts and pass the pointer (none of the game's 28 compute
     shaders reads it today; `cargo run --example cacheinfo -- build-ps4/ps4shaders.bin` in ps4/gnmlib lists each stage's
     user data).
   - **Mipmaps:** a mipmapped colour texture is laid out the hardware's way (addrlib SI/CIK, T# POW2_PAD): sizes padded to
     powers of two, level l's pitch = max(base pitch >> l, alignment), levels on 256 bytes, layers unpadded. A view of several
     levels is one T# from level 0 with BASE_LEVEL / LAST_LEVEL / BASE_ARRAY; a view of one level still points straight at
     it (independent of the hardware rule). **Open question for the console:** the pitch alignment of small levels, addrlib's
     max(8, 64 bytes) texels (rule a) or 64 texels / 256 bytes (rule b). Settled by runs 4-6 (below).
   - **8 / 16-byte clears** without a 4-byte period: a 64 KB block of repeated texels in the low heap, CP-DMA-copied over
     the target.
   - **Resolves were ignored** (found while looking at MSAA): with targets drawn single-sampled, the resolve target never got
     the picture. `end_render_pass` now copies each colour attachment to its resolve target (CP DMA, after a barrier).
   - **MSAA stays on, drawn single-sampled.** Turning it off would touch ~30 places and ~10 shaders in the shared PC renderer
     (merge cost). radv's GFX7 multisampled loads keep the sample index when the FMASK half of the descriptor is zero (ours
     is), so depth reads should come out right. Revisit only if the console shows artifacts.
   - **Console test: wgputest run 4** (`IV0000-RACR00006_00-RACRSWGPUTEST000`, rebuilt). Before its frames it draws a mip
     check twice, under rule a and rule b: two mipmapped textures (256x256 with 9 levels, 64x48 with 7) with a solid colour
     per level, sampled level by level into a 16x3 target through one multi-draw-indirect (first instance 0, 1) and one
     indexed indirect draw (first instance 2), plus an Rgba16Float (0, 0, 0, 1) clear, all read back. The log has a
     PASS / FAIL line per row and `checks: rule a ..., rule b ...`. PC Vulkan passes all of them. Whichever rule passes on
     the console becomes the only one.
     **Run 4 result (2026-11-07):** indirect draws WORK (multi-draw and indexed, first instance 1 and 2 picked the right
     texture), the Rgba16Float clear WORKS, the frames still equal the PC. Mips: rule a got the 64x48 texture right at every
     level and the 256x256 one at levels 0-6 and 8, but its level 7 (2x2) read black, so level offsets agree and that small
     level's pitch does not. Rule b is wrong. Solid colours hide most pitch errors and run b likely reused run a's freed
     memory, so the run cannot pin the rule down.
     **Run 5 (built): the mip address probe.** With `RAC_GNM_FILL_OFFSETS=1` (DEV, read at texture creation) the back end
     fills a mipmapped texture's memory with each word's own index and drops uploads into it (wgpu-core's zero init). A
     compute shader loads texels (0,0), (1,0), (0,1), (1,1) of every level of five textures (R32Uint 256x256 / 64x48 / 16x16,
     Rg32Uint 64x64, Rgba32Uint 32x32), so the log gives the hardware's base, texel step and row pitch per level, next to
     rule a's prediction (`probe K LN ...: base HW | ours, x step ..., row step ...`). Then the mip check runs once more.
     **Run 5 result:** every load read 0. Cause: wgpu-core zero-inits a texture without COPY_DST and with a renderable format
     through **render-pass clears** of each level (`map_texture_usage_for_texture`), not the buffer copy the back end drops,
     so the fill was wiped. One hint survived: Rgba32Uint 32x32's level 4 read memory past the end of our layout (non-zero
     garbage), so the hardware pads small levels of 16-byte texels more than rule a. **Run 6 (built):** the probe textures
     also get COPY_DST.
     **Run 6 result: the hardware's mip layout, measured.** Bases and pitches matched rule a everywhere except the smallest
     levels, where the hardware widens the pitch until **pitch x rows >= 64 texels** (one micro tile): R32 2x2 pitch 32
     (rule a 16), Rg32 4x4 16 (8), Rgba32 4x4 16 (8); level bases follow from those sizes. `format::mip_pitch` now applies
     that (rule b and `RAC_GNM_MIP_RULE` are gone), and its predictions equal every measured value. **Run 7 (built)** checks
     it: every probe line should read `base X | X` with equal steps, and `checks: PASS`.
   **Third part (2026-11-07 overnight): `ps4/app` builds for the PS4 and packages** (PKG RACR00007). The window loop is
   `rac-rs/src/viewer/ps4win.rs` (winit stand-ins), the heap is in direct memory, the PC-only deps are cfg-gated. Details,
   files and the console test steps: PS4_PORT_PLAN.md, "Step 5".

Rough size: the hal back end ~4-6k lines (the noop module is 900; the Vulkan back end ~12k does much more). The capture /
compile side ~1.5k. The PS4 app loop and platform gating ~1-2k.

## Facts to carry over

- **Shader registers that worked:**
  - `sceGnmSetVsShader` takes {pgm_lo, pgm_hi, rsrc1, rsrc2, spi_vs_out_config, spi_shader_pos_format, pa_cl_vs_out_cntl}.
  - `sceGnmSetPsShader350` takes {pgm_lo, pgm_hi, rsrc1, rsrc2, z_format, col_format, ps_input_ena, ps_input_addr,
    ps_in_control, baryc_cntl, db_shader_control, cb_shader_mask}.
  - llc does not fill RSRC2.USER_SGPR; set it ourselves.
  - The CB_COLOR0 tile mode index 8 means linear aligned. COMP_SWAP ALT matches the A8R8G8B8 display format.
- **The census overcounts render to texture:** it does not see uploads overwriting a drawn buffer. Only the "sample their
  own frame" figure is trustworthy.
