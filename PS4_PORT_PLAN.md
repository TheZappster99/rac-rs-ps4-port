# PS4 port (GoldHEN, PS4 Pro): plan and hand-off

## The game-code speed hunt (2026-11-11, rounds 18-25): the Modern camera makes the game do ~2.8x the work

### Answer (2026-11-11, night, PC fake only): not the camera, the PLACE. The pad script steers Ratchet somewhere else.
**Neither a modcam bug nor more on screen.** The left stick is camera-relative, so the same scripted stick (`1300-2900:0:80,0`) walks
Ratchet to two different spots: with the Modern camera (yaw held at 1.67) he runs straight +y into a wall at (267.8, 81.3, 45.9)
and stays there; with the classic camera retail's rig turns as he runs, he loops round and ends idle at (270.3, 56.2). The wall
spot is busy: ~350-400 skinned-moby calls (skin fast path, ~5,500-6,300 passes) a field against ~113 (~1,900 passes) at the classic
end spot. Evidence (new env-only diag `RAC_SKIN_LOG=1`, rac1 `worker/field_run.rs`: skin calls / passes / EE insns per field and
the hero position every 100 fields; `perf: insns last field` now prints 3 decimals):
- **Same spot, both cameras** (pad `...;1300-2200:0:80,0`, stick released at 2200, Ratchet idles by that wall in both modes:
  modern (266.7, 80.7), classic (267.3, 81.0)): modern 3.60-3.73 M insns a field, 345-359 skin calls; **classic 3.62-3.74 M, 379-402
  skin calls**. The "modern-only" blocks 0x290500, 0x2bc400, 0x293100, 0x290000 and the 3x skin-loop share all show up with classic
  there too (pc-sample shares within 0.5% of the modern run's).
- **Same scene, retail's pose put back** (temporary modcam diag, removed: once Ratchet was parked at the wall, modcam's record was
  replaced by retail's own pose every field, 25 deg of yaw and ~19 deg of pitch apart, eye 2.3 units away): still 3.0-3.9 M.
- The record modcam writes is sane: rows orthonormal, eye 6.2 units behind the pivot, nothing else in the 0x100-byte record differs
  from retail's (+0x40.. and +0x64.. are untouched; retail's +0x64 is the rig's own target near the hero).
- The earlier "modern step, retail record restored" run did match classic (1.27-1.34 M), but only because it also gave the hero
  retail's steering, so he walked the classic path. That confound is the whole story.

**What it means:** the console's round 24/25 "classic camera = 20.6 ms" is a cheaper spot, not a cheaper camera. Gameplay with
either camera costs ~41 ms a field at places like that wall (~3.6 M insns) and ~20 ms at quiet ones (~1.25 M). Full speed at busy
spots needs the game code itself to get faster (skin offload: skinning is the ~30% share and roughly triples at the busy spot), the
camera choice does not matter. Nothing was fixed or staged (no round 26). For camera-neutral measurements use the pad that ends at
2200 (same spot in both modes); for a quiet-spot baseline the default pad with camera=classic.

Goal: full speed (60 fields/s = 16.7 ms of game code a field). The user wants to stop the port if that is out of reach.
- **Round 18 (frames in flight):** works (present waits 0 ms); level 9.4 -> 13.2 fps. GPU then ~55-95 ms a frame.
- **Round 19 (settings):** water/waves off 13.3 -> 20.0 fps (biggest GPU cost); GI/AO/shadows off 16.1; fog/clouds and vegetation/
  draw ~nothing; all off 22.8 fps with the GPU idle (7 ms wait): then the game code (~45-48 ms a field, ~22 fields/s) is the limit.
- **Round 20 (PGO, commits e61f14e / eae1654):** game code 47.6 -> 45.0 ms (~5% on the console, 14% on the PC).
- **Round 21 (`RAC_PERF_LOG=1`, `RAC_PC_SAMPLE=1`, env only):** ~41 of the 45 ms is the `ee` section itself (snap ~2 ms, no waits).
  Hot pcs as on the PC: skin loop 0x216cd4 + its compiled blocks ~30% of busy samples; 0x118cb0 is the vblank poll (idle).
- **Round 22 (`RAC_PS4_CPUS=6/3/2`, fewer pools):** no change (41 ms). Note std reports 4294967295 cores without RAC_PS4_CPUS.
- **Round 23 (new package: `ee_oracle::EE_CPU_NS`, the game thread's CPU time, in the `retail: fields` line):** on its core 40 of
  41 ms: not preemption.
- **Probe rebuilt with today's code** (`ps4/probe`: path to `rac-rs/src/engine/gsfull`, stand-ins `gscap`/`level`/`render` in
  standins.rs): ~18 ms a field in the steady walk (fields 2030-2900), like Nov 8; a post-load stretch at 45-50 ms, then fast.
- **Round 24/25 (env only): the classic camera alone brings the app to the probe: game code ~20.6 ms, 47.7 fields/s (80% speed),
  on core 17.4 ms.** Mods off: no change. Debug keys off: no change.
- **PC fake reproduces it** (`camera=modern` vs `classic`, 4000-6000 fields, `RAC_PERF_ALL=1 RAC_PC_SAMPLE=1`): from field ~2100 both
  rise to ~6 ms; classic falls back to ~2.5 by 2400, modern stays at ~6 for good. **EE instructions a field: modern 2.79 M, classic
  1.00 M** (skin loop samples ~3x; blocks 0x2bc400, 0x290500, 0x293100, 0x2be800, 0x290000 run only with modern). Not the
  camera's own line tests (new counters `modcam::SEG_CALLS/SEG_INSNS`: ~2 tests, ~11 k insns a field), not draw distance
  (draw=low/medium/high all 2.79 M), not the pad script (left stick only).
- **Open question (next):** why the Modern pose makes the game itself run ~1.8 M more instructions a field. modcam overwrites the
  current camera record's rows/eye after the retail rig and the camera main publishes it, so the game's culling follows the modern
  pose. Either the modern view legitimately shows more animated characters (then it is content, and full speed with the modern
  camera needs the skin offload), or the pose makes culling / LOD / activation pass far more mobys than any real view would (rows
  not orthonormal or wrong scale, eye inside geometry, a stale field the frustum test reads): then it is a bug with a cheap fix.
  With the classic camera the remaining 20.6 -> 16.7 ms is about the skinning share (~30%): the skin offload would reach full speed.
- Uncommitted: `oracle/src/lib.rs` (EE_CPU_NS), `rac1/retail/tick.rs` (line fields), `rac1/retail/modcam.rs` (SEG counters),
  `ps4/probe` fixes, `ps4/console-tests/current-next` (round 25).

### Go / no-go on a 2x faster recompiler (2026-11-11, late, PC fake only): NO such lever. Nothing prototyped.
Busy spot (pad `...;1300-2200:0:80,0`, 4000 fields, `sh ps4/pc-prof.sh 44 14 4000`, RAM hash a1a30e19594f35f4, same with and
without the counters below): 3.6 M EE insns and 6.2 ms of game code a field on the PC (1.7 ns an insn; console 41 ms = 6.6x).
- **The recompiler's output is only ~22% of the game thread's busy time** (host sampling, 2,660 busy samples), spread over 177
  functions (the biggest 2.3%). The rest is already native: speed hands (`hs_hands`) 32%, `skin_ff` 20%, VU0 programs 6%, fpu
  helpers (mostly rsqrt for skinning) 5%, call dispatch / interpreter / hw 5%, memcpy for DMA 4%, mem 3%.
- **By EE instructions** (new diag counters `hs::IC_HAND` etc., printed as `ic-log` next to `skin-log`): hands 1.72 M (47%),
  skin fast path 0.92 M (25%, 5,500 passes x 166), recompiled code + interpreter ~1.0 M (28%). Cost on the PC: hands 1.2 ns an
  insn, skin 1.3-1.7, recompiled 1.7. Nothing is an outlier, so there is no cheap category to fix.
- So a generator change cannot cut 30%: recompiled code at zero cost would give 22-27%. What is in it: ~20 host insns of
  hand / hook checks plus a 7-register prologue at every function entry (`Cpu::hand`), ~8 per block for poll / icount / pc, a
  one-compare address test per access (already good), and the exact PS2 float ops (one VU0 `vmul` with flags outside a chop
  guard = ~90 host insns). ~40% of samples in hands, skin and recompiled code alike sit at SSE instructions: the exact float
  and 128-bit register work is the common cost, and upstream has already tuned it.
- **Reachable on the console (estimate):** generator tuning (entry check, flag liveness) ~-3 ms; skinning on another core
  ~-8..-12 ms: busy spots ~26-30 ms a field (33-38 fields/s). 16.7 ms would need every part about 2.5x faster at once. Quiet
  spots (20 ms, 25% skinning) can reach 16.7 with the skin offload alone.

## Frames in flight (2026-11-10, evening: built, round 18 measures)

Rounds 15-16 in the level: ~8.5-10 fps, "present waited for the GPU" 50-60 ms of a ~100-115 ms frame, GPU profile ~50 ms
(frame 900: water shade ~10 ms, level 9, volfog ~5, the rest small). `Queue::present` waited for the frame's last fence and only
then flipped, so the main thread could not record frame N+1 while the GPU drew frame N: CPU (~50 ms) and GPU (~50 ms) took
turns. The analysis is `build-ps4/ps4-logs/cpu-plan.md` (fix 1).
- **Change (gnm/mod.rs `Flipper`):** present hands (fence label, value, buffer, flip count) to a `gnm-flip` thread, which waits
  for the fence and flips (the vblank wait moves there too); present itself waits only for the PREVIOUS frame's flip, so at most
  one frame is in flight and the three display buffers are always shown / waiting / being drawn. `sceGnmSubmitDone` stays on
  the main thread in present. Presents that read GPU results (blit checks, snapshot rows, GPU profile frames, trace dumps,
  `RAC_PS4_SHOT` frames, `RAC_GNM_TRACE` frames) still wait the old way. `RAC_GNM_PRESENT=wait` = the old way for every frame.
  The back end's own memory is all fenced (submission words via `reclaim`, upload chunks freed when wgpu-core drops the command
  buffer, bind groups destroyed by wgpu-core after their submission), so nothing it writes can race the GPU.
- **New log lines:** `GNM: frames in flight: the flip thread waited X ms a frame for the GPU` (the GPU's time left after present
  returned); `retail: fields N/s, game code X ms a field (max), render thread waited Y ms a field for it` (rac1 `retail/tick.rs`
  `field_stats`, always on the console, `RAC_FIELD_STATS=1` on the PC). Reading them: main's "present waited" high = GPU bound;
  "render thread waited" high = game code (EE) bound; both low = render thread bound.
- **Open question:** round 9 found that after a SubmitDone the next submit blocked until the GPU finished. If that holds, the
  next frame's first submit absorbs the wait (`GNM: driver calls a frame: submit` grows) and the overlap is lost; then try
  SubmitDone on the flip thread after the flip.
- **PC fake check:** level walk 2500 fields, RAM hash equal to `RAC_GNM_PRESENT=wait` (1744e97c5bf29528). First try deadlocked
  ~4 s in: a present taken the old way moved the flip count on without a job, and the next present waited for a flip nobody
  would do; `Flipper::wait` now goes by the last job sent. The frames-in-flight line counts presents that waited ("presented
  waiting"): if that is high on the console, a diagnostic is cancelling the overlap.
- **Round 18** (`ps4/console-tests/current-next`): 1 level walk, frames in flight; 2 the same with `RAC_GNM_PRESENT=wait`
  (baseline); 3 frames in flight with `RAC_GSCAP_THREADS=3 RAC_THREADS=2` (core contention, cpu-plan fix 2); 4 hands on.

## GS capture on the GPU (2026-11-10: built, hangs on the console, CPU capture stays the default)

The game's 2D layer (gscap: HUD, menus, the pause pages' Ratchet preview drawn into VRAM fbp 0xd8) ran on the CPU rasterizer; the
pause Weapons page (~12,600 tiny triangles a field) crawled at ~1 fps on the console. The capture's GPU path can now run on the GNM device, but two of its kernels hang the console's GPU (rounds 16-17, below), so the
CPU capture stays the default:
- **Device:** `gscap/draw/gpu_shared.rs` `is_gnm` (Noop backend + driver "rac-rs GNM", console or PC fake): the window publishes its
  device; `RAC_GSCAP_GPU=1` runs the GPU capture on it (default: CPU). Its sharp second picture (lane G, 1440 x 1080 read back and
  uploaded every field) stays off on GNM (`hd_lane_allowed`; `RAC_GSCAP_HD=1` forces it): the 512 picture goes through `hud sharp`.
- **Readback:** `k_ov` op 3 (`OV_PACK`) packs the published picture to RGBA8 on the GPU (Rust `(c * 255.0 + 0.5) as u8`, soft-float
  exact) into 1 MB after the planes (`PACK_OFF`); the readback copies `512 * rows * 4` bytes straight into `Overlay::rgba` (was 4 MB of
  f32 copied, then converted on the MAIN thread by `ov_px`). Off with `RAC_GSCAP_OVHASH` (it hashes the f32 plane). The f32 path, when
  used, now converts on the gs thread too (`take_pic`). This also applies on the PC (any GPU capture).
- **Readback heap (hal):** MAP_READ buffers go to a 64 MB CPU-cached onion heap (`Shared::readback_heap`, gnmlib `Arena::new_onion`,
  memory type 0): CPU reads of write-combined garlic are uncached. Made on first use; console default on, `RAC_GNM_ONION=0` off; the
  fake uses it only with `RAC_GNM_ONION=1` (submission hashes unchanged otherwise).
- **Shader cache:** 26 kernel pipelines (13 kernels x `SOFT` 0/1: the float self test picks SOFT=1 when GFX7's float unit does not
  round like the CPU). Captured without the PC game: the app's PC build on real Vulkan, `RAC_PS4_CAPTURE=DIR RAC_GSCAP_GPU=own
  WGPU_BACKEND=vulkan [RAC_GSGPU_SOFT=1] rac-ps4-app.exe warm-gpu` (`build-ps4/capture-gs0`, `-gs1`), merged into `build-ps4/capture-v07`
  (157 pipelines, 41 compute, all compile, no scratch). Any gpu.wgsl edit re-keys all 26 (the key hashes the whole module).
- **Checks:** real GPU (`build-ps4/vkrun.sh`: the app's PC build on Vulkan, pause script, `RAC_GSCAP_OVHASH8=1`): the GPU capture's 1,144
  RGBA8 pictures equal the CPU capture's (459 distinct, Weapons page included; the GPU run has two extra frozen repeats at take-over).
  PC fake: no missing pipeline; level walk gs thread 8.4% of a core (CPU capture 13.1% + ~4.5% helpers); Weapons page 60 fields/s (CPU
  capture 22.5), gs 54% of a PC core: the rest is the VU1 model, VIF / GIF parsing and prim set-up (flat, no hotspot), so on the
  console (~6x slower cores) the page should be several times faster but may not reach full speed. Round 16 measures.
- **Round 16** (`ps4/console-tests/current-next`, staged in `build-ps4/to-console-next`): 1 GPU capture + Weapons page (pad: pause at
  field 2100, Weapons at 2180; on the console the level's cutscene ends by itself at ~1985, so Cross is pressed at 1700, 1760, 1900),
  self test, picture hashes; 2 the same with the CPU capture (hashes must equal run 1's: `grep ^ovhash8`, compare
  the hash column in order, allowing the take-over's extra frozen lines); 3 / 4 level walk GPU / CPU (fps; no RAC_PS4_PROF: suspected of the free-play freezes); 5 hands on.
- **Rounds 16-17 (logs `build-ps4/ps4-logs/round16`, `round17`): the GPU hangs at the save's loading screen** (field 493, ~34 s in; the
  hang check ends the app, no freeze), the moment the capture first draws prims. `RAC_GNM_CS_TRACE=1` (new: a drain after every compute
  dispatch writes its id to a label; the hang report prints the last finished dispatch, the next one in its encoder and its 16 params)
  named it: **`k_redo`** hangs (k_raster before it finishes); with k_redo skipped (`RAC_GNM_SKIP=k_redo`) **`k_serial`** hangs; with
  k_raster skipped, k_redo again. The cause is not proven, but k_redo and k_serial (both SOFT variants) are the only shaders in the whole
  cache that use **all 256 VGPRs** (`capbin.log`: v256; the most any console-proven shader uses is 128: hud sharp, water shade, k_hd),
  and nothing else differs (no scratch: SCRATCH_EN 0, "SPILL" is SGPRs into VGPR lanes; code 85-105 KB). Likely the PS4 keeps part of
  each SIMD's register file, so a 256-VGPR wave never starts. Making the workgroups wider so ACO caps VGPRs did NOT work: ACO (Mesa
  26.1.6 in WSL, Bonaire shim, which reports 256 physical VGPRs correctly) still emitted 256 VGPRs even for 1024-thread workgroups
  (which real GCN could never start), so their demand is not reducible by ACO; reverted. **Next, if wanted:** cut those two kernels'
  register demand in gpu.wgsl (the integer pixel pipeline inlined per prim: split it into smaller kernels or out-of-line steps), then
  recapture and run round 17's runs again (`ps4/console-tests/current-next`, runs 1/3 with the trace).
- **Float self test on the PS4:** the console's process runs with denormals flushed (MXCSR FTZ / DAZ; shadPS4 too), so the CPU reference
  is wrong for subnormals and every start fell back to the SOFT=1 kernels. `selftest` now leaves those cases out when the CPU flushes
  (operands, results, and op 14's intermediate product; the log line says how many). Round 17 still had 38 op-14 cases (fixed after).

Status (2026-11-10, morning): **round 13 is on the console, not yet run; round 14 is built and staged behind it** (see
"Overnight 2026-11-09/10" right below: the GPU's time was mostly the copy engine, and the title menu is the modern UI).

## Overnight 2026-11-09/10 (PC only, user asleep): the morning hand-off

**Order for the morning:** (1) run round 13 as staged (`build-ps4/to-console`, files already on the console; its PKG is in
`/data/pkg`). (2) Then round 14: `build-ps4/to-console-next` (made by `STAGE=next sh ps4/stage-test.sh <pkg>` from
`ps4/console-tests/current-next/`): copy `env.txt`, `ui_off.wgsl` and `runs/` into `/data/rac-rs`, the PKG into `/data/pkg`;
the user deletes the app, installs, starts it five times (`STEPS.txt`). Round 14's PKG has the same name as round 13's, so
upload it only after round 13 has run. Commits: 2d56ef2 (round 13), 9bec032 and 1b8cad7 (tonight).

**1. The GPU frame was mostly the copy engine (CP DMA), not shading.** Round 10's GPU profile has a pass that only clears
(`post fog cover clear`, 1920x1080 R8 = 2 MB) at 0.77 ms: CP DMA runs at **~2.7 GB/s** on the console. Every render-pass clear
and every resolve was CP DMA (`begin_render_pass` / `end_render_pass`), and at that speed the numbers match the profile:
sun shadow 3 x 16.8 MB depth clears = 18.6 ms (measured 19.5), water positions 33 MB Rgba32Float + 8 MB depth = 15.4 ms
(measured 14.7), "between passes" ~40 MB of resolve copies ~15 ms (measured 15), the level pass's own clears ~7 ms. The PC fake
now counts CP DMA per frame (`GNM: CP DMA a frame: fills X MB, copies Y MB (~Z ms of GPU at 2.7 GB/s)`, every stats line):
**level 7 walk: 198 MB a frame (~71 ms) before, 2.3-13 MB (~1-5 ms, uploads only) with the new draws.**

**The fix, `RAC_GNM_BLIT=clear|resolve|all` (rac-rs/vendor/wgpu-hal/src/gnm/blit.rs; off by default until round 14 proves it):**
- Clears are one full-screen triangle per target: colour by a pixel shader that exports the clear colour (32_ABGR, any
  float / unorm / snorm target, one layer; integer / sRGB / layered targets keep CP DMA), depth (and stencil, Replace with
  compare Always) by the depth-only pipelines' no-op pixel shader with the vertex shader writing z.
- Resolves (MSAA targets are stored single-sampled, so a resolve is a copy) are a triangle whose pixel shader `image_load`s
  the source at its own position (same format both sides, not sRGB / integer).
- Buffer clears of 256 KB or more (`clear_buffer`: the volumetric fog's per-pixel buffer, 8 MB every Enhanced frame) draw the
  buffer's 256-byte aligned middle as a linear R32Float target 2048 texels wide, the ends by CP DMA.
- The shaders are LLVM IR in `ps4/tools/blitsh/*.ll`, compiled by the toolchain's llc for gfx701 like the console-proven
  triangle test (`sh ps4/tools/blitsh/build.sh` prints the words `blit.rs` embeds; llc leaves RSRC2 USER_SGPR at 0, so the
  counts are set in `blit.rs`). The code block is allocated on first use, so with the switch off every other allocation and
  the command stream are unchanged: **ps4/wgputest's five submit hashes are identical to the committed build's** (checked with
  a worktree of 2d56ef2 at `build-ps4/wt-head`, since removed). With it on, the stream changes as intended.
- **`RAC_GNM_BLIT_CHECK=frames`**: after each blit in those frames a barrier and CP DMA copies of the target's middle row (and,
  for a resolve, the source's), compared at present: `GNM blit check <kind> <size> <format> in pass <label>: N of M match`
  (first difference printed). On the PC fake and in shadPS4 these show mismatches by design (neither runs draws); on the
  console every line must say all match. A clear colour that is not 0 or 1 may differ by one in unorm rounding.
- **`RAC_GNM_DMA_L2=1`**: the copies that remain go through L2 (DMA_DATA SRC_SEL / DST_SEL 3, CIK's TC_L2; radv / radeonsi do
  this on GFX7, Mesa `cp_dma_use_L2`; bit positions checked in Mesa's `pkt3.json`). Our barrier writes L2 back before the
  colour / depth units or the display read anything.
- Diagnostics: `RAC_GNM_DMA_LOG=bytes` logs every CP DMA of that size in any frame; the frame trace logs CP DMAs of 256 KB+
  (`RAC_GNM_TRACE_DMA=bytes`). What remains in a level frame: ~64 buffer uploads (16-473 KB, `queue.write_buffer`), the
  retail overlay's two 832 KB copies and a 1.2 MB upload per game tick.
- Risk: a wrong register in the blit draws can hang the GPU (power-off). Round 14 runs them in starts 3-5 only, after the
  comparison runs; STEPS.txt tells the user to skip the rest if one freezes.

**2. Water and shadows otherwise:** with water on screen (round 10 frame 900) `water shade` itself is 24 ms of real shading
(the reflection march: `waterfx/looks.rs` `march_steps` 16 / 24 / 32 for Water Low / Medium / High, plus wave trains). The
Water and Dynamic shadows settings already have cheaper tiers, so round 14 run 5 measures Medium for both once the clears
are cheap; choose the PS4 seed's defaults (`ps4/app/seed/pc_options.txt`) from that. Not done: tiled (non-linear) targets
and textures, the next GPU lever after the DMA.

**3. CPU frame:** `sh ps4/pc-prof.sh 28 14 3000` (the START / SECS window must end before the run's last field, or no
profile is written) with `RAC_GNM_BLIT=all`: the PC profile is flat (main 73%, encoder finishers 41% / 30%, ee 33%, gs 13% at
~200 fps; the top symbols are under 0.2% each, `user_data` / `graphics_user_data` still the biggest of the encoders). Nothing
safe and large to cut from the PC side tonight; round 13 run 3's console profile (RAC_PS4_PROF=120,60, before the blits)
and a profile after round 14 decide where the console's ~67 ms go.

**4. The invisible menu text: the title's logo and menu are the MODERN UI, not the HUD pipelines.** On the PC fake at
1920x1080 the title frames draw no `vs_hud` pipeline at all; `RAC_GNM_TRACE_PIPES=1` (new: every pipeline bound in a traced
frame) shows the menu in a separate pass "modern ui" (upstream v0.5.0's ui/gpu.rs, onto the screen after the level pass) with
pipeline `5f8e9bc6a1ac5831` "ui prims" (`vs_inst` / `fs_inst`: instanced quads, per-instance attributes, `draw(0..6, a..b)` with
a non-zero first instance, six `@interpolate(flat)` inputs, an R8Unorm 1024x1024 glyph atlas). So rounds 11-12 (hud sharp /
hud=original) changed pipelines the title does not use. The in-level HUD is the retail overlay quad (`20eff130d4bd87d3`
`vs_hud` / `fs_hud`, the 512x416 overlay texture), a different path; both are invisible on the console.
Checked and ruled out from the PC: blend factors / ops (match GFX7's register layout), the overlay / UI texture descriptors
(format, size, pitch, swizzle, clamp + linear sampler decode right), the R8 linear pitch rule (256 texels), LINE_STIPPLE-only
PS input enables (17 of 112 pixel shaders, all the full-screen post passes, which draw right on the console), stale L2 after
the overlay upload (every command buffer ends with a full barrier).
Fixed on the way (not proven to be the cause): **SPI_PS_INPUT_CNTL now follows radv**: flat inputs get FLAT_SHADE (bit 10),
explicit ones OFFSET bit 5 + FLAT_SHADE, interpolated ones nothing (radv_shader.c `radv_ps_input_type_bits`). The cache now
carries radv's float32 / float16 interpolation masks (PS header words 23 / 24, bit 31 of word 24 = present; `radvbin`),
re-packed from the existing `build-ps4/capbin` without recompiling (`build-ps4/ps4shaders.r13.bin` = the old cache; old caches
keep the old words; `RAC_GNM_FLAT_SHADE=0` reverts). ACO reads flat inputs with `v_interp_mov_f32 P0`, which should not need
FLAT_SHADE, so this may not change the picture. Round 14 decides:
- run 1 traces title frames 130 / 180 / 230: the "ui prims" binds and the UI pass's group 0 descriptors (the frame trace now
  prints the modern UI's pipelines and descriptors besides the HUD / overlay ones), and `RAC_GNM_DUMP_UPLOADS=8,100` saves the
  first 8 large texture uploads from frame 100 on, in any frame, as `trace_NNN.raw` (whole destination level, so a glyph
  upload shows its atlas): `python ps4/tools/raw2bmp.py trace_NNN.raw 1024 1024 1` (R8; now also writes PNG). On the PC fake
  the atlas reads "NEW GAME / LOAD / Veldin / Bolts / Play time ..." (`build-ps4/r14pc/trace_009.rgb.png`).
- run 2 refuses the modern UI (`RAC_UI_WGSL=/data/rac-rs/ui_off.wgsl`, not WGSL: the pipe guard turns the UI off and the game
  keeps the classic menu, drawn by `vs_hud` + the GS overlay; checked on the PC fake and in shadPS4) with `hud=original`. Text
  visible there and not in run 1 = the modern UI path; invisible in both = the overlay / texture path, which the in-level HUD
  shares.
- Round 13 (already staged) dumps the overlay upload (source and texture) at frames 180 / 700 and has an `RAC_OV_ASYNC=0` run.
Also noticed, not changed: the back end writes DB_SHADER_CONTROL = 0 and SPI_SHADER_Z_FORMAT = 0 for every pixel shader,
while radv sets KILL_ENABLE for shaders that can discard and Z_EXPORT_ENABLE + a Z format for shaders that write
`frag_depth` (the "overlay depth" pipeline, ov_depth.rs). The cache would need radv's `can_discard` / `writes_z` (radvbin).

**Round 14's five starts** (all with `RAC_GNM_GPUPROF=600..6000/150`, frame ranges are new: `A..B/STEP`, also for
`RAC_GNM_BLIT_CHECK`; pictures 130, 180, 230, 700, 1200): 1 the old way + UI trace + upload dumps; 2 classic menus +
hud=original; 3 `RAC_GNM_BLIT=all` + blit checks; 4 blits + `RAC_GNM_DMA_L2=1`; 5 blits + water / shadows Medium. Checked in
shadPS4 (runs 2 and 3: no crash, the UI refusal works, checks run). If run 3 shows all checks matching and the same
picture, make `RAC_GNM_BLIT=all` the default (blit::mode).

Status (2026-11-09, late): **rebased on upstream rac-rs v0.5.0** (see "Upstream v0.5.0"): new shader cache, PKG rebuilt
(same RACR00009), checked in shadPS4, round 8 restaged with runs 1-8 unchanged. Before that: **saving, options and the Mods page made ready for players** (see "Saving, options and mods"; round 8
runs 6-8 check them on the console). Before that, **the "shadows off hangs" finding was a test mistake** (see "Shadows off"; round 8 runs 4-5 test shadows
off for real). Before that, **speed work on the PC** (see "Speed work"): profilers for every thread on the PC and the console, the GNM
back end's per-draw user data and heap made cheap (encoder CPU a frame cut by about two thirds, command stream unchanged); round 8
gained two profiling runs. Before (2026-11-08, evening): **first console run of the real game (step 5 PKG): the level showed for ~1 s, very slowly, without the ground texture, then the whole console froze** (GPU hang, presumably). The log came back empty. **Round 3 gave the first console log** (6-9 fps CPU bound, GPU hang at the save's level load, some pipelines draw black; see "Diagnostics after the first run"); **the hang is fixed (scratch ring, confirmed in round 6); the screen is black from the title menu on; round 7 (per-pass picture rows) waiting for the console**, everything in `build-ps4/to-console` (`sh ps4/stage-test.sh <pkg>` stages `ps4/console-tests/current/` + the PKG). Before that: **step 6 built: controller, sound and saving** (second PKG, see "Step 6"), waiting for the console tests in `TESTS_FOR_TONIGHT.md`. (2026-11-07, overnight): **the real game builds as a PS4 PKG** (`build-ps4/app/IV0000-RACR00007_00-RACRSAPP00000000.pkg`, see "Step 5" below), waiting for its first console run. Earlier: (2026-11-06) **step 1 done.** The game core (EE + recompiled code, no picture, sound or pad) runs on the user's PS4
Pro. It boots the USA disc, loads the first save of `card.bin` with a scripted pad, and runs walking gameplay at **98-99% of
PS2 speed**. Apps quit cleanly. **Step 2 (the picture) part 1 is built and waiting for a console test:** the probe shows the
game's own picture through rac-rs's CPU GS-full pipeline and `sceVideoOut` (see "Step 2, part 1"). Nothing of this is
committed yet (see "Files").

## Black pictures: the cause (2026-11-09, night; fix built, waiting for a console run)

Round 8 run 1 (new build, pulled over FTP into `build-ps4/ps4-logs/round8b`): start-up scene as before (ties right, terrain /
sky / shrubs black), title menu all black. Test `ones3` (frame 55) made everything black, test `offs` (frame 70) changed every
colour: so the vertex colour fetch is what goes wrong.

**Cause: the vertex descriptor table is packed, and the back end indexed it by location.** radv's vertex shaders read the
descriptor of location L at slot `popcount(vb_desc_usage_mask & ((1 << L) - 1))` (radv_nir_lower_vs_inputs.c `desc_index`;
radv_write_vertex_descriptors writes them one after another). `vertex_table` wrote slot L. A shader that fetches every location
(most `vs` tie pipelines, mask 0b11111; wgputest's shaders) is unaffected; one that skips a location reads a zeroed descriptor
for the next one. `vs_tfrag`, 8 `vs` (shrubs), `vs_sky`, `vs_hud` / `vs_hudz`, the particle and starfield pipelines all skip
location 2 (masks 0b11011 / 0b1011), so their location 3, the vertex colour, read as zero: black terrain, sky, shrubs, and a
HUD / GS overlay with zero colour and alpha, which is the black title menu. `cacheinfo` now prints each vertex shader's mask
(`vb_mask`, `build-ps4/cacheinfo.txt`). **Fixed** in `vertex_table` (packed slots, table size = number of used locations).
A Mesa source tree to read radv in: `build-ps4/shadps4/src/externals/mesa-kosmickrisp/externals/mesa` (27.0-devel).

Also seen in that log: **the console's `available_parallelism` is 4294967295** (not only shadPS4's), so every helper pool takes
its upper limit. Round 8 runs 2 / 3 (system answer vs `RAC_PS4_CPUS=7`): no difference at all (7.5 fps title menu, 5 fps level).

**Speed: the main thread waits, it does not work** (round 8 run 2's profile, `sh ps4/prof.sh ... build-ps4/app/app.elf`). At
5 fps no thread is busy: ee 50% CPU, gs 31%, main 30%, vu1 17%. 59% of the main thread's samples sit in one system call
reached from a Sony library (pc 80000097a, return address 80049227e: not libkernel's own wrapper, not our code), while
"present waited for the GPU" is 0. So one of `sceGnmFlushGarlic` / `sceGnmSubmitCommandBuffers` / `sceGnmSubmitDone` (called
after every submission, 2-3 a frame) / `sceVideoOutSubmitFlip` blocks for ~120 ms a frame. **Round 9's build times each**
(`GNM: driver calls a frame: ...` before each stats line; `ps4::TIMES`). Also 11% of main was `pm4::split` walking every
submission: now only submissions over the limit are walked.

**Round 8 runs 4-8 (logs in `build-ps4/ps4-logs/round8b`): all pass.** Shadows off does not hang (run 4: the three shadow
volume pipelines first used in frame 642, ran 375 s; run 5 305 s). Run 6: `RAC_CARD_RESAVE` wrote card.bin at field 1400 (10 ms),
and after the PS-button quit run 7 restored it; card.prev.bin and a `history/` copy exist. Run 7: title Mods page opened,
Load mods off and on, active.txt intact (three mods). Run 8: a normal start, options back to the player's own (shadows high),
level 7 reached. Still missing: four `hud sharp` pipelines (74a38213968c9ee0, 5d19be71cc71ce38, 7811615c84caf4aa,
8908d112bf88c01a).

**Round 9 (on the console's /data/rac-rs since 2026-11-09 night):** run 1 the picture with the fix (shots 40, 300, 430, 600, 800, 1000) and the driver
timers; run 2 the same with sound on (first time). Run 3: `RAC_GNM_SUBMIT_DONE=frame` (sceGnmSubmitDone once a frame before
the flip, `plat::frame_done`, instead of after every submission): the first suspect for the stall. The profile's stubs, from
their callers: 80000169c `_umtx_op`, 8000012fc `sched_yield`, 80000097a/c the one main blocks in (also reached from
8004918b1 / 8004920be by main and `ov-write`, which both submit: the Gnm driver's ioctl, most likely).

Audit of the vertex fetch against radv's source (same night): per-attribute descriptors (our masks are by location, so radv
compiled with them), offset added by the shader, GFX7 record count, instance-rate index all match the back end now; the packed
table was the one mismatch. Not checkable on the PC: `textureLoad` on textures created multisampled but stored single-sampled
(the sample index lands in the LOD field of a one-level texture; fine only if the hardware clamps it).

**Round 9 result (`build-ps4/ps4-logs/round9`): THE LEVEL'S PICTURE IS RIGHT ON THE CONSOLE** (frames 600 / 800 / 1000: the
ship's landing scene, then Ratchet on the ground with terrain, sky, shadows, lighting and post effects). The packed vertex table
was the black-picture bug. Frames 40 / 300 / 430 came out black, but those moments are not the title menu: frame 40 is
field ~80 (before the title, the game's own fade, which the overlay now really draws) and 300 / 430 are fields ~650 / ~1000,
inside the save's level load (mode 3 -> 6 at field 493). The title and its menu live in fields 221-493, about 18-37 s in,
frames ~120-270: round 10 takes pictures there. Sound (run 2): `main port open 0x20000007`, no error in the log.

**Speed: the console is GPU bound.** Run 1's timers: `sceGnmSubmitCommandBuffers` 15 ms a frame in the menu, 173 ms in the
level, everything else ~0. Run 3 (sceGnmSubmitDone once a frame): submit 0.1 ms, and the same time shows up as "present waited
for the GPU" (63 ms at the end of the menu, 124 ms in the level); fps unchanged (5.5 in the level). So after a SubmitDone the
next submit simply blocks until the GPU has finished: the earlier "CPU bound, present never waits" reading was wrong. The GPU
needs 125-175 ms for a level frame at 1920x1080 with the seed's settings. Once a frame is now the default
(`RAC_GNM_SUBMIT_DONE=submit` = the old way).

**Round 10 (on the console since 2026-11-09 night):** `RAC_GNM_GPUPROF=450,700,900` (`Shared::gpu_prof_mark`: an end-of-pipe
event at each pass's start and end makes the GPU write its clock counter, DATA_SEL 3; the log lists each pass's share of the
frame), pictures 130 / 180 / 230 / 280 / 700. Run 1 the player's settings, run 2 every costly option off
(`lighting=classic,ao=off,shadows=off,water=off,gi=off,vegetation=off,clouds=off,fog=off,sunrays=off,particles=low,fur=off`;
checked in shadPS4: no new missing pipeline) as the speed floor. Suspects for the GPU cost besides the effects themselves:
colour targets and textures are linear (not tiled), which this GPU renders to and samples slowly.

**Round 10 result (`build-ps4/ps4-logs/round10`):**
- **The title scene is right** (frames 130-230: stars, planets, terrain, the hangar) **but has no logo or menu text**, and the
  level has no HUD: those quads take the two `hud sharp` pipelines (hudsharp.rs, on when the surface is 1080 high:
  `retail::ui_sharp(height)`), which the PC capture never made because its window was 1280x720. Their draws were the
  "draws skipped" of every log since round 1. **Fixed:** `ONLY=seed RAC_TEST_SIZE=1920x1080 sh build-ps4/capture-v05.sh`
  added exactly 7811615c84caf4aa and 8908d112bf88c01a; cache rebuilt (144 pipelines; both spill 36 KB of scratch a wave,
  which the scratch ring covers); shadPS4: no missing pipeline. **Capture at the console's 1920x1080 from now on.**
- **The GPU profile works** (1 tick = 10 ns: run 2's 1.5 M ticks = its 15 ms present wait). Level frame 700, the player's
  settings, 102 ms: sun shadow (3 passes) 19 ms, level 19, between passes (47 gaps: copies, clears) 15, water positions 15,
  cloud march 5, water bed 4, sea mask 3, water mirror 3, particles 3, fog apply 2.5; AO / GI / bloom are each under 1 ms.
  Frame 900, 123 ms: water shade 24, sun shadow 20, level 18, between 15, water positions 15. So water (~45 ms) and the sun
  shadow maps (~20 ms) are the GPU's cost, not the post chain.
- **Run 2 (every costly option off): 15 fps in the level, GPU 15 ms a frame** (the bare `level` pass 12 ms). There the CPU
  is the limit: ~67 ms a frame. Title menu: 7.5 fps with the GPU at 16 ms, also CPU (the EE's 38-66 ms a field in the menus).
  So the road to a playable speed is both: cheaper water / shadows on the GPU, and the CPU frame (EE + renderer) after that.

**Round 11 (on the console):** the cache with `hud sharp`; pictures 130 / 180 / 230 / 260 (title, menu, save list), 700 / 900
(the level's HUD). Sound is left for a start with the TV's sound on (`RAC_PS4_AUDIO=1`; round 9 run 2 opened the port fine).

**Round 11 result (`build-ps4/ps4-logs/round11`):** the two `hud sharp` pipelines run (no draw skipped, the scratch ring is
made for 7811615c84caf4aa at frame ~150) but **still no logo, menu text or HUD** in the pictures or on the TV. `fs_hud_sharp`
keeps its 6 x 6 texel block in a private array, which radv puts in scratch memory (36 KB a wave); with every element zero it
returns alpha 0, which is exactly "nothing drawn". So the suspect is the scratch ring, never proven to work on the console
(the only other user is `sky model`). Checked against radv: the ring table (base, BASE_ADDRESS_HI | SWIZZLE_ENABLE),
SPI_TMPRING_SIZE (WAVES | KB << 12) and RSRC2's SCRATCH_EN (in the cached rsrc2, 0x7) all match; not checkable from here:
whether `sceGnmSetPsShader350` keeps SCRATCH_EN. Also found, harmless so far: `ps_input_cntl` takes its flat mask from
radv's `explicit_shaded_mask` (not `flat_shaded_mask`) and sets bit 5 instead of FLAT_SHADE (bit 10); ACO reads flat inputs
with `v_interp_mov`, so nothing shows it.
**Round 12 (same package, on the console):** `RAC_PS4_PCOPT=hud=original`, the plain HUD pipeline. The title menu takes about
35 s to appear on the console (the user's timing).

**Round 12 result (`build-ps4/ps4-logs/round12`): still no logo, menu text or HUD with `hud=original`** (the user watched;
pictures 130-230 show the title scene only). So it is not (only) the sharp shader's scratch memory: the plain `vs_hud` /
`fs_hud` quads are invisible too. **Open, the next thing to trace.** What is known: the HUD pass runs and nothing is skipped;
the level's `hud` pass resolves the picture to the screen correctly; a faint dot grid over the sky and a darker strip about
22 pixels wide at the left edge are visible in the title pictures, so some overlay quads do land. Ideas, cheapest first:
1. `RAC_GNM_TRACE=180` on the console (per-pass draw counts, target rows, group-0 dumps) to see whether the HUD quads are in
   the `level` pass at all and what the pass's row looks like after them.
2. The HUD textures: glyph / art textures are created and filled per frame (`queue.write_texture` -> staging buffer -> CP DMA
   copy into a texture laid out the hardware's way). A wrong layout or a copy that lands in another mip / pitch would leave
   them zero (transparent). `RAC_GNM_AB_SKIP` / `RAC_GNM_AB=1` (blending off in a frame window) would show the quads as
   solid rectangles if they are drawn with zero alpha.
3. `fs_hud` = texture x vertex colour (location 3, Float32x4 at offset 32): the same fetch the level uses, so less likely.
4. Compare with the PC fake at 1920x1080: `RAC_GNM_TRACE` there lists the same passes and the buffers' real contents.

**Round 13 (new PKG RACR00009 built 22:12, on the console with env + runs 1-3):** in retail mode the logo, menu text and HUD are
all the GS overlay (`RETAIL_OV_TEX`, 512 x 416/448 RGBA8, uploaded every tick by the `ov-write` helper: CPU -> staging ->
persistent buffer -> texture, the only chained copy), so `hud=original` could not change anything. The frame trace now saves every
upload of 64 KB or more whole (`trace_NNN.raw` in the run's shot folder, source and texture; `python ps4/tools/raw2bmp.py FILE
WIDTH ROW` -> colour and alpha BMPs) and prints, for every `vs_hud*` pipeline bound, its key and the group-1 descriptors (24
words: T# at 0, S# at 16; checked on the PC fake: format 8_8_8_8 unorm, sizes, pitch, swizzle, clamp + linear all right). Run 1
traces frames 180 / 700, run 2 the same with `RAC_OV_ASYNC=0` (upload on the render thread), run 3 hands on: scripted pad off,
sound on, `RAC_PS4_PROF=120,60`, GPU timers in frames 1200 / 1500 / 1800. Overlay alpha is transmission (0xff = see-through), so
an all-zero texture would make the screen black, not invisible text.

**Hand-off (2026-11-09, end of the night).** Works on the console: start-up, title scene, save load, the level's picture,
saving, options, Mods page. Open: (a) logo / menu text / HUD invisible (above); (b) speed: GPU 100-125 ms a level frame
(water ~45 ms, sun shadows ~20 ms), CPU ~67 ms a frame behind it; (c) sound never listened to (`RAC_PS4_AUDIO=1`, port opens);
(d) scratch memory unproven on the console. On the console now: round 12's files and the round 11 package (in `/data/pkg`).

## Upstream v0.5.0 (2026-11-09, late)

ps4-port = mods-migration (now on upstream v0.5.0) + the PS4 work. What changed for the PS4 build:
- **Three crates.** v0.5.0 split the game into `rac-formats` (src/formats), `rac-engine` (src/engine) and `rac1` (src/rac1). The
  PS4 pieces moved with it: `rac-rs/src/engine/viewer/ps4win.rs` (engine viewer.rs re-exports its `Window` on freebsd; rac1
  `host.rs` takes `EventLoop` / `ActiveEventLoop` / `ApplicationHandler` / `WindowId` from it), `rac-rs/src/rac1/retail/ps4pad.rs`,
  and `write_replace` / `write_synced` now live in `rac-rs/src/engine/durable.rs` (memcard.rs re-exports them) because the
  engine's own files (pcopts, mods settings, texture-pack store) use them too. The freebsd dependency split (no cpal, wgpu with
  only `noop`) is now in `rac-rs/crates/{engine,rac1,formats}/Cargo.toml`.
- **New PC-only code fenced off:** upstream's FSR / DLSS upscaler and its Vulkan device hooks (`gpu/dlss_vk.rs`,
  `upscale/dlss*.rs`, `upscale/ffx*.rs`) use wgpu's Vulkan / DX12 internals; their `target_vendor = "apple"` fallbacks now cover
  `target_os = "freebsd"` too (`ffx_frame.rs` `record_vk` has a freebsd stub). ps4win's `Window` gained `request_inner_size`.
- **The vendored wgpu-hal and the wgpu / naga versions did not change**, so the GNM back end and the capture hooks carried over as
  they were. `ps4/app/Cargo.lock` gained the new crates only.
- **Shader cache re-captured** (`build-ps4/capture-v05`, script `build-ps4/capture-v05.sh`; the old cache is
  `build-ps4/ps4shaders-v043.bin`): 142 pipelines (114 render, 28 compute), all compile for GFX7. Two things the capture now needs:
  - **`RAC_BINDLESS=0` for one run.** The PC draws ties through the bindless texture array; the GNM adapter has no binding
    arrays, so the PS4 draws them with the plain tie pipelines, which v0.5.0 builds lazily (metalpipe.rs `Pipes`, per Metal
    set) and the PC therefore never creates. Without that run the fake run missed 6 `vs` / `fs_tie` / `fs_tie_lt64` pipelines.
  - **The full 3000-field walk** (the pc-prof route reaches another part of level 7 after field 1800).
  - The 38 option-sweep variants (`build-ps4/optsweep`) add no pipeline: v0.5.0 builds every option's pipelines at start-up.
- **Checks:** PC fake (`sh ps4/pc-prof.sh`): level 7, 3000 fields, **no missing pipelines**, also with `RAC_PS4_PCOPT=shadows=off`.
  shadPS4 (120 s): title -> Load Game -> level 7, ~40 fps, only the known two `hud sharp` missing.
- **Gotcha: line endings.** With `core.autocrlf=true`, switching from mods-migration (no ps4/) back to ps4-port re-created every
  file under ps4/ with CRLF, and `git apply` then refused `ps4/std.patch` (the build stops at "ps4/std.patch did not apply";
  the console run files and seed settings would have gone out with CRLF too). `.gitattributes` now keeps `/ps4/**` at LF. If it
  happens again: delete the ps4/ files and `git -c core.autocrlf=false checkout -- ps4`.
- **Not updated:** `ps4/probe` includes `rac-rs/src/gsfull/mod.rs` by `#[path]`; that folder is now `rac-rs/src/engine/gsfull`, so
  the old core probe needs its path changed before it builds again (the app does not use it).

## Saving, options and mods (2026-11-09, checked in shadPS4 and on the PC fake; round 8 runs 6-8 carry the console check)

- **Saving survives the PS button.** The console caches writes and the PS button kills the app without exit code, so on the PS4
  (`target_os = "freebsd"`) every save and settings file goes through `retail/memcard.rs` `write_replace`: temp file written and
  fsynced, renamed over the old one (if the rename cannot replace it: remove, rename again; `restore` then falls back to
  `card.tmp` / `card.prev.bin`), then the folder fsynced (a failure is logged once). `card.prev.bin` is a synced read + write
  instead of `fs::copy`. Used by card.bin, pc_options.txt, mods/active.txt and mods/settings.json. The PC keeps its plain write
  and rename. The log line now gives the time (`memcard: saved ... (N ms)`; 12-14 ms in shadPS4). **DEV `RAC_CARD_RESAVE=N`**
  writes the card once after N fields, the save path without scripting a real save (a picture run of the PC game to design one
  stalled: the windowed exe logs nothing).
- **Options are kept.** `ps4/app` `pc_options_for_run` used to copy `pc_options.orig.txt` over `pc_options.txt` at every start
  once a test run had made it, which would have thrown away the player's choices for good. Now the stash exists only between a
  test start (`RAC_PS4_PCOPT`) and the next normal start, which puts it back, empties it and deletes it (shadPS4's unlink reports
  success and keeps the file, so an empty stash counts as none). Checked in shadPS4: test start -> normal start restores ->
  a player's `ao=low` survives the next normal start.
- **Graphics options and the shader cache: nothing to hide.** `sh ps4/pc-prof.sh` (now with `RAC_PS4_SHADERS`, `RAC_RETAIL_PAD`
  overridable) ran every value of every Graphics row (34 values changed alone from the seed, plus all-lowest, all-low and a mix):
  **no missing pipeline in any** (level 7, title -> load -> walk, 2400 fields). A run with no cache lists 100, so the census works.
  So no option is locked and no re-capture is needed; other levels may still need pipelines (listed in missing_pipelines.txt as
  before). The PC-only rows (Renderer, Display mode) do nothing on the PS4 (`WGPU_BACKEND=noop` always wins).
- **Mods page on the title (PS4 only).** `pcext::mods_title()` turns on upstream's title Options -> Mods page (`pcmods.rs`, its own
  RAM `BASE+0xC000..0xD800` and text ids `0x7fc0..0x8000`) when `cfg!(target_os = "freebsd")`; the pause page stays off (it only
  has texture packs and live script-mod settings). The page lists the three built-in mods (from their manifests) with their
  settings rows, writes `mods/active.txt` / `settings.json`, applied at the next start. Ray tracing is still absent (not in
  `PLUGINS`). DEV `RAC_MODS_TITLE=1` shows the page on the PC (the fake run of ps4/app), `RAC_FRONT_LOG=1` logs each front-end page
  and cursor change; the log now says `title Mods page opened / closed`. Scripted path (USA): Start 220, Down 280, Down 300,
  Cross 340 (Options), Down 400/420/440, Cross 480 (Mods), Down 560 (the "Load mods" row; row 0 is the page stepper), Cross flips it.
- **shadPS4 series** (`EMU_KEEP=1 sh ps4/emu-run.sh - SECS` keeps settings, mods and the run series between starts; ENV `-` leaves
  env.txt): shadows-off test start; normal start with `RAC_CARD_RESAVE=1400` (saved, renamed over the old card, `card.prev.bin`
  and `history/` written, emulator killed by its timeout afterwards); Mods page start ("Load mods" off and on, two writes); normal
  start (card restored, three mods on, shadows high). PC fake with the seed: level 7, no missing pipelines.
- **Round 8 runs 6-8** (STEPS.txt): 6 = the save (`RAC_CARD_RESAVE=1400`), quit with the PS button after 4 minutes, also the first
  start after runs 4-5 without a test option; 7 = the Mods page by pad (with `RAC_FRONT_LOG=1`); 8 = a normal start. Copy-back adds
  card.bin, card.prev.bin, history/, pc_options.txt, mods/.

## Speed work (2026-11-09, PC only; round 8 carries the console check)

Console runs so far: 6-9 fps at the start-up scene and in the title menu, 3-5 fps in the level, present never waits for the GPU
(CPU bound). Even the start-up scene, where the game code is light, gets ~7 fps, so the renderer's CPU side is the cost.

**Profilers.** `ps4/app` has a sampling profiler for every thread on both platforms, switched on by
`RAC_PS4_PROF=START,SECONDS` (written to `RAC_PS4_PROF_OUT`, default `profile.txt` in the player folder; a test-series run
writes `/data/rac-rs/profile_runN.txt`). `sh ps4/prof.sh profile.txt <exe or ELF>` makes the table ("thread  function"; `THREAD=name`
keeps one thread, `TOP=N`). Header lines give each thread's CPU % over the window (and on the console the cores its samples ran on).
- PC (`src/prof_win.rs`): suspends each thread ~1000 times a second (Toolhelp + GetThreadContext), CPU % from GetThreadTimes.
- PS4 (`src/prof.rs`): std's spawn hook (`add_spawn_hook`, set first thing in main; `feature(thread_spawn_hook)` on the PS4 build
  only) notes every thread; a sampler sends SIGPROF to each with `pthread_kill` every 4 ms; the handler notes rip / caller (words
  28 / 31 of the context, as the probe found), the core (`rdtscp` TSC_AUX) and its CPU clock (CLOCK_THREAD_CPUTIME_ID). Threads
  clear their slot as they end (a thread-local guard: shadPS4 asserts when a dead thread is signalled). Checked in shadPS4 (runs,
  no crash), but there the signals arrive on host threads, so its addresses mean nothing: only the console gives a real profile.
- **`sh ps4/pc-prof.sh [START] [SECS] [FIELDS]`:** the app on the PC fake GPU with the console's seed settings, card and pad script,
  profiled, table printed (`build-ps4/profrun`). It sets `RAC_GNM_FAKE_DMA=0` (the fake's CPU-side DMA copies, which the console's
  GPU does, are skipped) and `RAC_HERO_GPU=1` (the console's hero self-check passes, the fake's cannot).

**Core count.** The game sizes its helper threads by `available_parallelism` (GS capture threads, encoder-finish split, picture
tuner...); fewer than 3 turns the encoder split off, so the two `enc-finish` threads' work lands on the render thread. In shadPS4
std reports 4294967295 (sysconf failing as -1, read as unsigned); the console's answer is unknown. The log now says
(`ps4: cores: std reports ...`), and `RAC_PS4_CPUS=N` forces N through our std (`rac_std_set_cpus`, `ps4/std.patch`,
sys/thread/unix.rs).

**PC profile (fake GPU, level 7 walk), per frame of CPU on the PC:** before: render thread ~4.8 ms, `enc-finish-0` ~5.9 ms,
`enc-finish-1` ~4 ms (98 fps with the fake DMA off). The biggest single cost was our own back end: `user_data` /
`graphics_user_data` (per draw, VS + PS) allocated a dozen small vectors each time (~37% of the encoder threads).
**Fixed:** they build on the stack now, same register writes and uploads in the same order. The heap's free list (`mem.rs`) was a
sorted vector shifted on every free (3.8% of the render thread); now a BTreeMap walked in the same order (first fit hands out the
same ranges). After: render thread ~3.5 ms, encoders ~2 + 1.5 ms, **~207 fps** (the console does roughly 7x less per core: about
45 ms less CPU a frame there). **Both changes leave the command stream byte-for-byte identical:** `RAC_GNM_SUBMIT_HASH=1` prints
an FNV hash of each fake submission (with the fake heaps at fixed addresses so runs compare), and ps4/wgputest gives the same five
hashes with the old and the new back end.

What is left on the render thread is spread thin (props draw lists, dynamic lights, skinning, wgpu-core's per-draw tracking,
interpolation); no single item above ~6%. Whether the console runs the threads side by side is the open question: if it does, the
render thread (~25 ms on the console) bounds it near 40 fps; if they share a core, the sum (~50 ms) does.

**Round 8 (staged in `build-ps4/to-console`) now has three starts:** run 1 the picture test as before, run 2 a profile in the
level (100-130 s) with the console's own core count, run 3 the same with `RAC_PS4_CPUS=7`. Next, from those: the cores column says
whether threads spread over the cores; the per-thread table says what to move off the render thread or speed up next.

## Shadows off (2026-11-09, PC only; round 8 runs 4-5 carry the console check)

Round 5 concluded "`shadows=off` causes a hang of its own" from run 5. **That run never had shadows off**, and its hang has a
different cause:
- Its own log says `dynamic shadows high` (the game's read of pc_options.txt), although the app had logged `graphics option
  shadows=off`. In shadPS4 the same code applies the option (`dynamic shadows off`), with and without `pc_options.orig.txt`, so
  why the console's file lacked the line is unknown.
- On the PC fake GPU (`RAC_GNM_LOG_FIRST=1`, new: every pipeline's first use goes to the log; `pc-prof.sh` now takes
  `RAC_PS4_PCOPT` and `PC_RUN=DIR`) a real shadows-off run never uses `vs_sh` / `vs_sh_props`, nor wgpu's indirect-draw
  validation compute (`50690b80737b0963`). Run 5 made `vs_sh` at the level load, so it drew the sun shadow map.
- The sun shadow map is the only user of indirect draws in that scene (`sunshadow.rs` `multi_draw_indexed_indirect`; the
  validation compute appears only when shadows are on). wgpu's validation compute writes the arguments those draws read. Run 5
  listed `50690b80737b0963` in `RAC_GNM_SKIP`, so its dispatches were dropped and the shadow draws read whatever the buffer's
  memory held: huge counts, a GPU hang named after `vs_sh` (the only new pipeline in the report; skipped ones are not listed).
  Run 4 skipped it too (and had no scratch ring then). Run 1 (shadows off alone) was before the scratch fix, so the `sky model`
  explains it.

What shadows off really changes: no shadow map pass, no indirect draws, and the level draws the moby **shadow volumes**
(`shadowvol.rs`: three `shadow volumes` pipelines, `636cb63e2a17d665` increment, `782ea258becc1cb5` decrement,
`6a26ef4b31397a88` darken; first used ~250 fields into the scripted walk on the PC). They are the first draws that make the depth
unit read and write **stencil** (the main depth target's stencil plane) and the first pixel shaders with no colour export
(`col_format 0`, `exp null done`, checked by decoding the 12-byte shader; `cacheinfo` now prints each PS's export setup and
`cacheinfo FILE KEY DIR` writes a pipeline's machine code). All three are in the cache. The stencil encodings (DB_DEPTH_CONTROL,
DB_STENCIL_CONTROL op values, DB_STENCILREFMASK front / back with STENCILOPVAL 1, DB_STENCIL_INFO / read / write bases, the
stencil plane's layout and clear) match the GFX7 register tables. No hang shows on the PC; only the console can say.

Changes: `RAC_GNM_SKIP` never skips wgpu's own pipelines (logs it once instead), so a bisect can no longer make that hang. The
app writes `RAC_PS4_PCOPT` without `fs::copy`, logs any failed write and reads the file back into the log
(`ps4: /data/rac-rs/pc_options.txt now: ...`); `PcOptions::load` also applies `RAC_PS4_PCOPT` itself on the PS4, so a run's
options hold whatever the file says. **Round 8 run 4:** shadows off, nothing skipped, first uses logged (5 minutes, to reach the
volumes). **Run 5:** the same with the shadow volumes skipped, the fallback if run 4 hangs once they appear (then the stencil is
the suspect). Checked: the skip guard on the PC fake; the PKG (rebuilt, same title id) in shadPS4 with run 4's settings, 150 s,
no crash, option applied and read back.

## Mods in the PS4 build (2026-11-09)

The PS4 build has three built-in mods only: Hoverboard, Ragdoll Ratchet and the wrench slam (`slam-shockwave`), all on in the
seed (`ps4/app/seed/mods/active.txt`). Ray tracing is not in its `PLUGINS` list (`plugins.rs`, `target_os = "freebsd"`), the
`RAC_RT_*` switches do nothing there (`rtshadow::part`), and a ray-tracing manifest left in `/data/rac-rs/mods` by older builds
is removed at start-up. Checked in shadPS4: the log lists the three mods, and RT switches in env.txt change nothing.

## shadPS4 on the PC (2026-11-09, started)

Goal: a first check of a PS4 build on the PC so fewer rounds need the console. shadPS4 v0.21.0 (official win64 release,
github.com/shadps4-emu/shadPS4) is in `build-ps4/shadps4/emu/`; its `/data` is `build-ps4/shadps4/data` (holds `rac-rs/game.iso`,
a copy of the user's USA disc). **`sh ps4/emu-run.sh [env.txt] [seconds]`** clears the old logs, runs `build-ps4/app/pkg/eboot.bin`
and copies our `log.txt`, the emulator's log and any shots into `build-ps4/shadps4/last/`. Git Bash needs `MSYS_NO_PATHCONV=1`
or `/data` becomes a Windows path (the script sets it).

Two changes that are harmless on the console were needed:
- `std.patch` (`stack_overflow.rs`): shadPS4 reports a zero guard size for every thread; std panicked "there is no guard page"
  in each new thread. Now one page is assumed on freebsd, as std already does for old musl.
- GNM back end `upload_code`: a 32-byte "OrbShdr" footer after each shader (code length, FNV hash). Sony's compiler writes it;
  the GPU ignores it; shadPS4 finds a shader's length and identity only through it.

**Patched shadPS4** (`ps4/shadps4.patch` on tag 0.21.0, built by `ps4/build-shadps4.cmd` with VS 2026's C++ workload + Clang,
installed 2026-11-09; `emu-run.sh` uses it when built): adds `S_BFM_B64` (ACO emits it; Sony's compiler never does) and drops
the "NOP hint is missing in CB setup sequence" assert (our CB packets carry no such hint).

**Result: with shadPS4's null GPU (now `emu-run.sh`'s default) the app runs the full 4 minutes, 60 fps at the title screen,
~14,200 fields, EE ~10 ms/field, no crash; `missing_pipelines.txt` names the same two "hud sharp" keys as the console.** So
start-up, loading, game logic, threads, memory and the pad can be checked on the PC. Pictures are black there and
`hero_gpu`'s self-check falls back to the CPU path (no GPU results); both are expected.

**GPU path (`EMU_GPU=1`): blocked.** The first compute shader (`cs_0xbeb3680cf9c33ce9`) builds a buffer descriptor whose
address is `SelectU64(cond, user SGPR0-1, set dwords 8-9)`: ACO folded two descriptor choices into a run-time select, and
shadPS4's resource discovery accepts only descriptors read straight from user data or memory (`IsSharpSource`). Making that
work means teaching shadPS4 run-time-selected descriptors (a real change to its design), so the picture stays a console job.
Also stubbed in shadPS4 and worth knowing: `_umtx_op` (returns 0: our std's futex waits become spins, slow but working),
`_fcntl`, `sysctl`, `cpuset_getaffinity`; `scePadInit` is a stub (no controller input).

## Diagnostics after the first run (built 2026-11-08 evening, waiting for the console)

**Console run 1 (step 5 PKG):** picture for about a second in the level, very slow, ground (tfrag) untextured, then a full console
freeze (PS button dead, power button needed). `log.txt` was 0 bytes: the console's file cache never reached the disk, while
`missing_pipelines.txt` (opened, appended, closed early on) survived: `hud sharp` x2 (two pipelines the PC fake run never made;
probably the 1920x1080 surface's HUD scale; their draws are skipped, harmless).

Checked on the PC fake GPU (same recipe, `build-ps4/fakerun3`): the biggest submission is ~2 MB of PM4 (a level frame, 2-3
submissions a frame), the low heap peaks at 2 MB of 64, no missing pipelines. So neither the CP's 4 MB indirect-buffer limit nor
a full low heap explains the hang; it is something only the real GPU does (a fault in a pass that never ran on the console:
3D / array textures, the 28 compute shaders, storage writes, tiled depth...).

The diagnostics PKG, **`build-ps4/app/IV0000-RACR00009_00-RACRSAPP00000003.pkg`** ("RAC-RS PS4 (diagnostics)", `APP_VARIANT=3`):
- **Log survives a freeze:** `ps4/app` fsyncs fd 2 once a second, after a panic and at exit; the back end fsyncs after its first
  five `GNM: frame N shown` lines and after each stats line (`plat::sync_log`).
- **`GNM: stats` once a second:** fps, how long present waited for the GPU (GPU-bound when that is most of the frame),
  submissions and KB of PM4 a frame, low / main heap use and peak, draws skipped. Low-heap high-water lines every 4 MB.
- **GPU hang:** present and `wait_for_idle` give up after 2 s, log `GNM: GPU HANG` with the frame count and sizes, sync the log
  and `abort()` (on the console; `RAC_GNM_HANG_ABORT=0` keeps running). Before, `wait_for_idle` spun forever and present returned
  Lost and the game went on submitting.
- **Submissions split** into command buffers of at most `pm4::MAX_SUBMIT_DWORDS` (0xF0000) on packet boundaries
  (`pm4::split`, unit-tested; the fake submit checks every stream parses). Not needed at today's sizes, a guard for bigger ones.
- **`RAC_GNM_SKIP=part,part,=exact,KEY`** (env.txt): pipelines whose label contains a part (or equals `=exact`, or whose key is
  listed) are treated as missing, so a pass can be switched off on the console to bisect a hang or a cost. Labels: the `label`
  lines of `build-ps4/capture/*.rec`. Ready-made files: `ps4/console-tests/current/`.
- **`RAC_PS4_SHOT=30,100,...`:** those presented frames saved as `/data/rac-rs/shot_NNNNN.bmp` (BGRA copy of the display buffer,
  written on a thread; `RAC_PS4_SHOT_DIR` elsewhere).

**Round 2 result:** `log.txt` and `log.prev.txt` 0 bytes again, so the fsync was not the problem: **`dup2` of the log file onto
descriptors 1 and 2 does not reach the file on the PS4** (std ignores EBADF on stdio, so nothing complained). Shots arrived
(written through their own files): frames 30 and 100 identical, the viewer's start-up preview of level 0 (Veldin) before the
retail game: **ties (bindless, blending off) drawn and textured; tfrag, sky and the "vs" shrubs (all blended, per-draw texture
bind group) black**; frame 300 all black (the retail game's own start; logos / movie, or the GS capture's 2D layer drawing
nothing); the console froze before frame 1000. `missing_pipelines.txt`: `hud sharp` x2 again.

**Round 3 build (2026-11-08 late):**
- **Log through std itself:** `ps4/std.patch` now also patches `sys/stdio/unix.rs`: stdout / stderr write to a descriptor set by
  `rac_std_set_log_fd(fd)` (no_mangle in std), `rac_std_sync_log()` fsyncs it. `ps4/app` opens `log.txt`, hands its fd over
  (`into_raw_fd`, never closed); `plat::sync_log` calls `rac_std_sync_log`. If the file cannot be created: `log_error.txt`.
- **`RAC_GNM_AB=1`:** `Shared::ab_mode` overrides each bound pipeline's state by frame count: 45-59 blending off, 60-74 depth /
  stencil off, 75-89 both (shots 40 / 55 / 70 / 85 in `current/env.txt`). Blend and compare encodings were re-checked against
  sid.h and look right, so the pictures decide between blending, depth and the per-draw texture path.

**Round 3 result (2026-11-08 night): the first real console log** (`build-ps4/ps4-logs/log.txt`, 214 lines; the std stdio
hook works).
- Start-up fine: heap in direct memory works (988 MB), shader cache found, pad opens (`ps4pad` user / handle lines), card.bin
  restored, the front end runs and is drawn by the mirror (title scene), only `hud sharp` missing (draws skipped 10-96 a second
  in the menus: those two pipelines).
- **Speed: 6-9 fps, present never waits for the GPU (0.0 ms), so CPU bound.** EE alone 5 ms a field at first, 28-69 ms a
  field in the title menu (fields 300-1100). PC frame ~150 ms on the Jaguar cores; the renderer's CPU side is the big cost.
- **Hang:** right after the front end left (field 1161, the save loading its level), `GNM: GPU HANG: a frame did not finish
  in 2 s` at 492 frames; the app aborted itself (the hang check works).
- **A/B pictures:** depth test off (frame 70) makes the black shapes vanish and shows what is behind them: the black ground,
  arch and foreground are drawn, in black, and write depth. Blending off (55, 85) blacks out the whole screen (a final
  see-through layer then covers it; the test was too blunt). So: some pipelines output black. Candidates: props (`vs_props`,
  storage buffers group 2), terrain (`vs_tfrag`), the generic level shader (`vs`), the sky (`vs_sky`: texture x vertex colour).

**Round 4 build:** `RAC_GNM_AB_SKIP=FROM-TO:part|part;...` (`pipe::ab_skipped`) hides matching pipelines inside frame windows;
`current/env.txt` hides props, terrain, `=vs`, sky, bindless in turn (shots 50-90). The hang report now also lists the pipeline
keys first used in each of the last 8 submissions that had new ones (`GNM: first used in frame N: KEY ...`; labels in
`build-ps4/capture/KEY.rec`).

**Round 4 result:** same hang at the same point (492 frames, the save's level load). The pipelines first used in that
submission: wgpu's indirect-draw validation compute (`50690b80737b0963`), `vs_sh` (sun shadow map), `sky model`, `cloud noise`
x3 (compute, 3D noise), `sky backdrop`, and the full-screen chain `fs_lin fs_ao fs_blur fs_march fs_resolve fs_apply fs_gsrc
fs_gi fs_giblur fs_bright fs_down fs_up fs_comp`. Pictures: hiding `vs_tfrag` removes the black arch / ground (so **terrain draws
black**); hiding props, sky or bindless changes nothing (not drawn in that scene); hiding `=vs` removes the ties, which look right.
The terrain's texel alpha survives (it is not discarded), so the colour is lost in the shading: `tfrag_rgb` -> `sun_baked` ->
`sun_occ`. Suspect: the sun shadow map is never rendered in the front end (`vs_sh` first runs at the level load), so it holds
junk and near terrain reads as fully shadowed (distant rocks lie outside the cascades and look right).

**Round 5 build: a test series in one go.** `ps4/app`: `/data/rac-rs/runs/runN.txt` (N from `runs/next.txt`, advanced at start)
is applied after env.txt; the log is `log_runN.txt`, pictures go to `shots_runN/`. `RAC_PS4_PCOPT=shadows=off,...` rewrites
pc_options.txt for that start (original kept as `pc_options.orig.txt`, restored every start). Runs: 1 shadows off, 2 clouds /
sky model off, 3 full-screen chain off, 4 indirect validation off, 5 all of them.

**Round 5 result:** only run 2 (`cloud noise`, `sky model`, `sky backdrop` skipped, shadows on) got past the level load and kept
running (level 7, 4-5 fps, present still never waits for the GPU). Runs 1, 3, 4 hung as before; run 5 (clouds off but
`shadows=off`) hung too, with `vs_sh` the only new pipeline: **`shadows=off` causes a hang of its own** (wrong: that run still had
shadows on and skipped wgpu's indirect-draw validation; see "Shadows off"). Shadows off did not change the black terrain at all (run 1's frame 40 is bit-identical), so the sun-shadow
theory was wrong. **From the title menu on, every picture is all zero bytes** (frame 300 in the menu, 600 / 800 in the level),
alpha included: only the start-up preview ever reached the screen.

**The hang's cause:** `cacheinfo` (now printing `scratch`) shows exactly one cached shader with scratch: `55a7b10686210119`
(`sky model`, fs_reduce) at 72 KB a wave, which got the zero table instead of a scratch ring. **Fixed (round 6):**
`Shared::scratch_tmpring` makes a 32 MB ring on first use (256 waves x 128 KB), the ring table (base, BASE_ADDRESS_HI |
SWIZZLE_ENABLE, as radv's preamble; ACO builds the rest) goes to AC_UD_SCRATCH_RING_OFFSETS of spilling shaders, pipelines set
SPI_TMPRING_SIZE / COMPUTE_TMPRING_SIZE (WAVES | KB << 12). `RAC_GNM_NO_SCRATCH=1` skips spilling pipelines instead.

**Black screen:** `RAC_GNM_TRACE=frames` logs every pass (targets, SCREEN mark, resolves, draw counts), compute pass, texture
copy and the presented buffer. On the PC fake GPU at 1920x1080 (`RAC_TEST_SIZE`): the start-up preview and the title menu are
one `level` pass resolved straight to the screen; in the level the `level` pass resolves off-screen, the post / water / cloud
passes follow, and a `hud` pass loads the MSAA colour and resolves it to the screen. (The fake marks the MSAA target SCREEN
too: it reuses a freed surface buffer after the PC window's resize; the console never resizes.) Round 6 traces frames 300 and
650 on the console.

**Round 6 result: the hang is fixed.** Run 1 (scratch ring on): `GNM: scratch ring 32 MB ... first for pipeline 55a7b10686210119
(sky model)`, level 7 loaded, ran 318 s (3-5 fps, CPU bound) with no hang. Pictures still all black (B G R 0, alpha 255) from the
title menu on. Traces: the title-menu frame is the same single `level` pass resolved to the presented display buffer as the
start-up frame (6966 draws); the level frame is the full chain ending in `post composite` into the MSAA colour and a `hud` pass
resolving it to the screen. So the resolve reaches the right buffer and the black comes from what is drawn. The PC fake (now
carrying out CP DMA on the CPU, `plat::fake_dma`, so copied uniforms hold real data) reads the same camera data in frames 40
and 300. Suspect: a `vs_hud` overlay first used at frame 250 (`fa62f5f3b96f6792`, colour = src + dst * src alpha: the retail
GS overlay) whose texture or alpha comes out zero on the console, wiping the scene.

**Round 7 build:** the frame trace also copies (CP DMA) the middle row of each pass's first colour target after the pass and
prints how much of it is not black (`GNM trace row ...`), and dumps the first group-0 buffers each pass reads
(`GNM trace dump ...`), once the frame is done.

Next, from what round 7's log says: a hang names the moment (frame count) and the bisect files name the pass; the stats line says
whether the slowness is the GPU (present wait) or the CPU; the shots show the ground.

## Step 6: controller, sound, saving (built 2026-11-08, not yet run on the console)

Same app, second PKG so both install side by side: **`build-ps4/app/IV0000-RACR00008_00-RACRSAPP00000002.pkg`** ("RAC-RS PS4 (pad,
sound)", `APP_VARIANT=2 sh ps4/pkg.sh app`). Step 5's PKG is kept unchanged in `build-ps4/app-baseline/` as the first console test.
Both use `/data/rac-rs`. The user's test list: `TESTS_FOR_TONIGHT.md`.

- **Controller (`rac-rs/src/rac1/retail/ps4pad.rs`, `retail.rs` names it `hidpad` on the PS4):** `sceUserServiceInitialize` +
  `sceUserServiceGetInitialUser`, `scePadInit`, `scePadOpen(user, 0, 0, NULL)`, retried every second until it opens;
  `scePadReadState` per `state()` call, turned into an `XRaw` (Cross A, Circle B, Square X, Triangle Y, L1 LB, R1 RB, Options
  Start, touchpad click Back = Select, L3, R3, d-pad; L2 / R2 from the analog values), so the PC's mapping, remapping and prompts
  apply. The scripted pad (`RAC_RETAIL_PAD`) is still set and ORs in during its windows; `RAC_RETAIL_PAD=` in env.txt removes it.
  Log lines `ps4pad: ...` (init results, user, handle, connected / disconnected, read errors).
- **Sound (`audio.rs`, `Audio::new` on freebsd):** `sceAudioOutInit`, `sceAudioOutOpen(0xff system user, MAIN, 0, 256 frames,
  48000, S16_STEREO)`, a thread `audio-out` that runs the PC mixer into 256 stereo frames, converts to i16 and calls the blocking
  `sceAudioOutOutput` (the port paces it, like cpal's callback). `RAC_PS4_AUDIO=0` keeps the game silent. Log line `audio: PS4 ...`.
  Untested guesses: the system user id 0xff for the port, S16 stereo (float formats were left out on purpose), that a second
  `sceUserServiceInitialize` (pad + audio both call it) is harmless.
- **Saving:** nothing new; `retail/memcard.rs` already writes `card.bin` (plus `card.prev.bin` and timestamped `card_*.bin`) into
  `RAC_PLAYER_DIR` = `/data/rac-rs`. Untested on the console (rename over an existing file).
- `ps4/build.sh` links `-lScePad -lSceUserService -lSceAudioOut` for the app. Checked on the PC: the Windows build type-checks
  (lib + tests); the PS4 ELF imports the new functions. Not run anywhere: the PS4 code paths only exist on the console.

## Step 5: the real game as a PS4 app (built 2026-11-07 overnight, not yet run on the console)

`ps4/app` (the game exe's `main` + `rac_game` + the generated `ee-game`, linked against the vendored wgpu-hal with `ps4-gnm`) now
builds for the PS4 target and packages as **`build-ps4/app/IV0000-RACR00007_00-RACRSAPP00000000.pkg`** ("RAC-RS PS4", ~190 MB:
eboot, `stubs.txt`, `ps4shaders.bin`, `hand_skip.txt`, first-start settings). Build: `sh ps4/build.sh app && sh ps4/pkg.sh app`
(10 min the first time, ~3 min after a rac-rs change).

- **What runs:** cold boot from `/data/rac-rs/game.iso`, the scripted pad of the design's step 5 recipe (Start 220, Down 280,
  Cross 320 and 420 = Load Game, first save of `card.bin`; left stick up 1300-2900), drawn by the real renderer through the GNM
  back end onto the 1920x1080 surface. No sound (`Audio::new` is None on the PS4), no controller.
- **PC check (fake GNM GPU, same exe source, `RAC_RETAIL_SMOKE=3000`, the PS4 settings, `ORACLE_EE_STACK_MB=32`):** title -> Load
  Game -> level 7 -> 3000 fields, exit 0, **no `missing_pipelines.txt`** (every pipeline in the cache), GPU heap peak 960 MB.
- **The PS4 start-up (`ps4/app/src/main.rs`):** stdout + stderr -> `/data/rac-rs/log.txt` (previous run: `log.prev.txt`); first
  start copies `pc_options.txt`, `mods/active.txt`, `mods/settings.json` from the PKG (`ps4/app/seed`, the settings the shader
  cache was captured with: lighting enhanced, vegetation lush, GI high, draw high, the three PS4 mods); then the environment
  Play.cmd sets (ORACLE_ISO/STUBS/HAND_SKIP, RAC_PLAYER_DIR and RAC_MODS_DIR = `/data/rac-rs`, ORACLE_EE_STACK_MB=32,
  WGPU_BACKEND=noop + WGPU_NOOP_BACKEND=1, RAC_RETAIL_PAD, RAC_THREADS=4), overridable by **`/data/rac-rs/env.txt`** (`KEY=VALUE`,
  `KEY=` removes; e.g. `RAC_PS4_FRAMES=3000` stops after 3000 frames). A memory line every 10 s (flexible memory free, heap size).
- **Program heap in direct memory (`ps4/app/src/heap.rs`):** dlmalloc as the global allocator, growing in >= 64 MB segments of
  WB_ONION direct memory (CPU cached), never returned. The ~280 MB of flexible memory (musl malloc) looked far too small for the
  game's CPU-side data. Untested on the console: the first log lines say whether the mapping works.
- **Window loop (`rac-rs/src/engine/viewer/ps4win.rs`, since v0.5.0):** stand-ins for winit's `EventLoop`, `ActiveEventLoop`, `Window`,
  `WindowId` and the `ApplicationHandler` trait. viewer.rs picks them on the PS4, so handler.rs runs unchanged: `resumed` once,
  then `RedrawRequested` in a loop. The "window" hands wgpu a dummy raw handle (the GNM surface ignores it), 1920x1080, 60 Hz.
- **Quitting:** after `rac_game::main` returns the app calls `rac_ps4_shim::quit`. Paths inside the game that end with
  `std::process::exit` (the smoke check `RAC_RETAIL_SMOKE`, a few dev tools) probably crash the app like returning from main
  does; use `RAC_PS4_FRAMES` on the console instead of the smoke check.
- **Link fixes:** crates.io libc (winit -> rustix calls `kevent`) names FreeBSD symbol versions the PS4 lacks; `build.sh` makes
  `build-ps4/libc-ps4` (the registry copy with `@FBSD_*` stripped) and ps4/app patches libc to it (**run build.sh once before a
  PC build of ps4/app**). The shim stubs `posix_spawn_file_actions_addchdir(_np)` (std's `Command`; create-fself refuses
  unresolved imports). PkgTool fails on files in folders create-gp4 does not list, so the seed files ship flat (`seed_*`).
- **What to look for in the first console run:** the log's first `ps4:` lines (heap, flexible memory), `gpu:` adapter lines,
  `GNM:` heap lines, `retail: travelled to level 7`, any `PANIC`. Likely trouble: flexible memory (thread stacks: ee 32 MB,
  retail 16 MB, snap / vu1 16 MB each, bgpool capped at 8 MB per thread, rayon 4 x 2 MB), the GPU work itself (first time the
  full renderer's command buffers run on the GPU: a hang would freeze the app and may need a hard power-off), speed (the
  renderer and the game share the CPU: expect it to be slow).

## Step 2, part 1: the first picture (built 2026-11-06, not yet run on the console)

- `probe/src/gsf.rs` runs rac-rs's GS-full (`rac-rs/src/gsfull`: VIF1, VU1 microprograms, software GS, CRTC read), compiled
  into the probe **unchanged** through `#[path = "../../../rac-rs/src/gsfull/mod.rs"] mod gsfull;`. Its only reaches into rac-rs
  (`crate::retail::disc`, `crate::texpack::discover`) are answered by `probe/src/standins.rs`. It needs `rayon`.
  - Our own two-stage pipeline instead of `gsfull::pipe::start`, for smaller thread stacks (VU1 stage 32 MB, raster stage
    16 MB; the PC's are 64 MB each, too much for flexible memory). Blank VRAM/VU1 as a cold boot does. Jobs come through
    `m.cpu.mem.gsq`, one `FieldIn` per field after `m.frame()`; the EE blocks while the pipeline is a field behind.
  - A PPM snapshot every 1000 fields (`frame_NNNNN.ppm` in the log folder); the log gets a GS-full line per report.
  - Raster workers: `gs_threads.txt` / `PROBE_GS_THREADS`, default 3 on PS4.
- `probe/src/vout.rs`: `sceVideoOutOpen` (user 0xff), three 1920x1080 A8R8G8B8-sRGB linear buffers in one 2 MB-aligned
  direct-memory block (type 3, prot 0x33), flip events on an equeue. A presenter thread shows colour bars, then each new
  picture stretched nearest-neighbour to 1440x1080 (4:3) with black sides. `build.sh` links `-lSceVideoOut`.
- **PC check (3 raster threads):** the picture is right (Veldin, Ratchet on the walkway at field 2000). But GS-full is slow:
  raster + read 27-50 ms per field and VIF1+VU1 1-13 ms, so the game ran at 30-60% speed **on the PC**. At ~6x slower per
  core, the PS4 should manage only a few pictures a second. So this PKG proves video out and the picture's correctness; it is
  not the playable path.
- **Console run (2026-11-06, user quit at field 393, title screen):** video out works: `sceVideoOutOpen` and buffer registration
  succeeded and the TV mode reads 1280x720 (the console scales the 1080p buffers). Flexible memory free: 180 MB after GS-full
  started, 134 MB once the game thread was up. GS-full on the console: raster + read **260-325 ms** a field (3 workers), VIF1+VU1
  **62-65 ms**, so about **3 pictures a second** and the game at 5-6% speed (it waits ~200-250 ms a field for the pipeline).
  That is ~6-10x the PC's times, as expected. **The user saw the game's picture on the TV** (correct, just slow), and the console's
  `frame_01000.ppm` (the starfield load screen of the save) is **byte-identical** to the PC run's: the PS4 runs the game and
  GS-full deterministically, exactly like the PC. The CPU
  picture is confirmed as a correctness check only.
- **What to decide after the console run:** a CPU-drawn picture cannot reach full speed on the Jaguar. The playable path is
  the GPU: either port the wgpu renderer's drawing to a PS4 API (GNM, or Piglet GLES2, which OpenOrbis ships a sample for and
  which needs precompiled shaders), or run GS-full's raster stage on the GPU. Measured PS4 stage times from this run will size
  that choice.

## Step 2, part 2: GPU feasibility test (built 2026-11-06, not yet run)

`ps4/gpu` (`sh ps4/build.sh gpu && sh ps4/pkg.sh gpu`, PKG `IV0000-RACR00003_00-RACRSGPUTEST0000`, log `/data/rac-rs/gpu.log`).
Piglet only takes PSSL binaries in OpenOrbis's sample (WebKit's precompiled set); the open question is run-time GLSL ES
compilation. The test:
- loads `libSceShaccVSH.sprx` (the system shader compiler, which the browser's WebGL should use) from the sandbox
  `common/lib`, `/system/common/lib` or `/data/rac-rs`, **before** Piglet, and logs each attempt;
- loads `libScePigletv2VSH.sprx` and resolves every GL / EGL call with `sceKernelDlsym` (no link-time imports, so missing
  pieces are logged rather than breaking app start), makes the context with the sample's config, and logs GL strings,
  extensions, `GL_SHADER_COMPILER` and binary formats;
- compiles a PS2-style modulate shader from source; on failure shows a colour cycle (swap-chain proof);
- on success times 120 frames each of: render-to-texture (512x512 FBO), then 2k-60k blended textured triangles per frame in
  1 call or batches of 5-30 with a texture + blend change per call, with a fresh vertex upload per frame.
**Run 1 (2026-11-06):** no `libSceShaccVSH.sprx` in any of the three folders (0x80020002, not found). Piglet loaded, but
EGL did not start (`eglInitialize failed`, error 0x3000, so probably `eglGetDisplay` gave no display) when GL / EGL were
looked up with `sceKernelDlsym`. The user saw an instant crash. **Run 2 (built):** GL / EGL linked directly as the sample does
(`build.sh` adds `-lScePigletv2VSH -lScePrecompiledShaders` for `gpu` only), each EGL step logged, the system library folders
listed (looking for any shader compiler), and all of WebKit's precompiled shaders dumped to `/data/rac-rs/shaders/`. Those
binaries matter if there is no run-time compiler: they are then the only PSSL at hand, to study or adapt.
**Run 2 result (2026-11-06):** `eglGetDisplay` returns null with direct linking too (`scePigletSetConfigurationVSH` returns true,
EGL error stays 0x3000). The app then shows an error at exit (probably Piglet's threads at `quit`). The sandbox `common/lib`
has 439 modules (logged in the run) but **no shader compiler at all**: only `libSceGLSlimClientVSH/ServerVSH` (a remote GL
pipe for the shell), `libSceJscCompiler` (JavaScript) and `libScePrecompiledShaders`. The 82 precompiled shaders (copies in the
user's `Downloads\PS4\shaders`, never commit them) are WebKit / shell UI shaders: textured quads with uniform alpha, colour
filters, blurs, solid colour. None does per-vertex colour, so they cannot draw the PS2's Gouraud-shaded triangles.
Their format: a 0x24-byte header, `Shdr` block, GCN machine code from file offset 0x80, then an `OrbShdr` footer with the
names of uniforms, attributes and varyings (`__GLOBAL_CB__`, `samplers2D[0]`, `S_POSITION`, ...).

**Decision point: Piglet is dropped, GNM is the route.** Rust's own LLVM (`llc` in the private toolchain) has the AMDGPU
backend and compiles LLVM IR shaders (amdgpu_vs / amdgpu_ps, `llvm.amdgcn.interp.*`, `image.sample`, `exp`, like Mesa's
radeonsi) for `-mcpu=gfx701` (the PS4's GCN 1.1 "Liverpool"). A test PS2-modulate pixel shader compiled, and its encodings match
the console's shaders byte for byte (`s_wqm_b64` 7e0afebe, `v_interp_p1_f32` ...c8, export f8...). `llvm-objdump` cannot
disassemble GCN in this build (no disassembler component), which is fine. With GNM (`libSceGnmDriver`, in OpenOrbis's lib
folder and on the console) shaders are just machine code plus register values in PM4 command buffers we write ourselves, and
`sceVideoOut` (already working) shows the render target. References for the registers and packets: AMD's Sea Islands ISA
and register docs, Mesa radeonsi (CIK paths), and the shadPS4 emulator (it parses exactly what a PS4 game submits).
**Next test (step 2, part 3): one GNM triangle** — a direct-memory render target registered with video out, a PM4 buffer that
sets the colour target, viewport, VS/PS (llc-built) and draws 3 vertices, `sceGnmSubmitCommandBuffers` + `sceGnmSubmitDone`,
an end-of-pipe label to wait on, then flip.

## Step 2, part 3: GNM triangle (WORKS on the console, 2026-11-06)

**Result:** every driver call returned 0 (`sceGnmDrawInitDefaultHardwareState350` returns 0x100 = dwords written), the GPU
wrote the end-of-pipe label after 0.6-0.9 ms, 600 frames ran in 10.0 s (60 fps, vsync-bound). Pixels read back are exactly
what the shaders compute: background ff0d0d40 (0.05, 0.05, 0.25 in A8R8G8B8: R and B land right, so COMP_SWAP ALT is
correct for the display format), triangle centre ff555555 (1/3 each). So all the untested guesses below held: tile mode index
8 = linear aligned, RSRC3 CU mask 0xffff, the shader register structure orders, no extra prologue before a submit.
**We can draw with the PS4 GPU using our own shaders and command buffers.**

### Overnight work, 2026-11-06 (no console needed; the user was asleep). Nothing committed.

- **Waiting for the console, run in this order:**
  1. **Probe PKG** (VU1-only mode by default). Gives the VU1 stage's cost in a level: decides whether option A stays a
     fallback.
  2. **GNM test 2 PKG** (`build-ps4/gnmtest/IV0000-RACR00005_00-RACRSGNMTEST0002.pkg`). WGSL shaders compiled by Mesa radv
     for GFX7, drawn through the new GNM layer: a textured, tinted background plus a spinning vertex-buffer cube with depth.
     It first tries to map memory below 4 GB (radv's GFX7 shaders need it for descriptor sets) and logs every attempt.
     **Run 1 (2026-11-07):** the low arena mapped at exactly 0x80000000 (MAP_FIXED). Frame 0 drew exactly right: the tinted
     texture checks (ff151524) and the cube's red face in the centre (ffff3333), with depth. **Frame 1 hung** (no end label
     after 2 s, its target untouched). That points at something new since GNM test 1: ACQUIRE_MEM, the depth target, or
     texture / vertex fetches, or a GPU fault in frame 0 after its label. **Run 2 (built):** the frame grows in phases of 3
     frames (empty, + ACQUIRE_MEM, + colour target and background without depth, + depth, + cube), with a label after the
     start, the background, the cube and the end, so the log names the phase and the point where it stops.
     **Run 2 result:** phases 0-2 run fine (empty, ACQUIRE_MEM, textured background). The first depth frame finished too,
     but the second one hung inside the background draw, and the app could not quit (hard power-off). Cause (very likely): GFX7's
     DB takes the depth layout from `DB_DEPTH_INFO` (ARRAY_MODE, PIPE_CONFIG, banks), not from `DB_Z_INFO.TILE_MODE_INDEX`
     (Mesa sets the index only on GFX6), and we never wrote it. **Run 3 (built):** `target::depth_z32f` writes DB_DEPTH_INFO
     = ARRAY_MODE 2 (1D thin) | PIPE_CONFIG 12.
     **Run 3 result (2026-11-07): WORKS.** All 600 frames, every phase, every label; the cube's red face (ffff3333) over
     the tinted background with depth testing, quitting cleanly. **WGSL shaders compiled by radv on the PC, with descriptor
     sets below 4 GB, vertex buffers, a texture, a sampler, a uniform buffer and a depth buffer, run on the PS4 GPU.** Next:
     PS4_GPU_DESIGN.md work order step 3 (pipeline capture + compile + cache file).
     **Step 3 done (2026-11-07):** 141 captured pipelines compile for GFX7 and pack into `build-ps4/ps4shaders.bin`
     (`sh ps4/tools/shcache.sh`). Next: step 4, the wgpu-hal GNM back end.
     **Step 4, first version (2026-11-07, built, not yet run on the console):** the GNM back end
     (`rac-rs/vendor/wgpu-hal/src/gnm/`), and `ps4/wgputest`, the wgpu program that tests it. Build and run:
     `sh ps4/build.sh wgputest && sh ps4/pkg.sh wgputest`, then compare `/data/rac-rs/wgputest.log` with the Vulkan
     reference pixels in PS4_GPU_DESIGN.md.
     **Console run 1 (2026-11-07):** wgpu runs on the PS4 GPU. The heaps, the cache (4 of 4 pipelines) and the 1920x1080
     surface all came up. **The background pixels equal the PC Vulkan render exactly** (`ff7b89cb`, `ff7a88c9`). It ran 600
     frames in 10.0 s (vsync) and quit cleanly. The cube drew (depth and geometry right) but came out black. Cause: radv's
     GFX7 vertex shaders take **one descriptor per vertex attribute** (by shader location: buffer address + attribute
     offset, the buffer's stride, records = (size - offset - format size) / stride + 1), not one per buffer.
     **Run 2:** the cube had colour, but wrong (grey where the PC is red). The shader adds the attribute offset itself, so
     the descriptor's base must be the plain buffer address (the record count still uses offset + format size).
     **Run 3 (2026-11-07): WORKS.** Every pixel sampled on frames 0, 1 and 120 equals the PC Vulkan render
     (`ffdf8282` / `ff5a3131`, `ffff9595`, `ff315a5a` / `ff5a5a31`), at 60 fps with vsync. So the following all work on the
     console through wgpu: textures, sampler, uniforms, vertex and index buffers, immediates, depth, DMA clears and copies,
     readback, and the surface. **Next:** run the real renderer's pipelines through the back end. Start on the PC fake GPU
     (how many of the game's pipelines hit the cache), then build the rac-rs PS4 app loop.
     Run 1 of the probe: see PS4_GPU_DESIGN.md, measurement 1 (VU1 26-32 ms a field in the level).
     **Real game on the fake GNM GPU (PC, 2026-11-07): WORKS.** `ps4/app` links the whole game against the GNM back end. It ran
     title -> level 7 -> 3000 fields with every pipeline found in the shader cache and 960 MB peak GPU memory. Four back-end
     fixes, the key-dump tool and the list of what is still dropped (MSAA, mipmaps, indirect draws, one clear) are in
     PS4_GPU_DESIGN.md, work order step 5. **Next:** indirect draws and mipmaps in the back end, MSAA off for the PS4 build and
     a re-capture with its settings; then build `ps4/app` for the PS4 target (platform gating of winit / cpal / the Windows
     paths, the PS4 app loop).
     **Back-end gaps filled (2026-11-07, PC):** indirect draws and dispatches, hardware mip chains, 8-byte clears and
     render-pass resolves (PS4_GPU_DESIGN.md, step 5 second part). MSAA stays on, drawn single-sampled (no re-capture).
     **Waiting for the console:** wgputest run 4 (the mip / indirect / clear check, two mip pitch rules). **Then:** keep the
     passing mip rule, and build `ps4/app` for the PS4 target.
     **Run 4:** indirect draws and the 8-byte clear work on the console; mips almost (a 2x2 level's pitch differs).
     **Run 6:** the probe measured the real mip layout (small levels: pitch x rows >= 64 texels); the back end follows it.
     **Waiting for the console:** run 7, the check of that. **Then:** build `ps4/app` for the PS4 target.
- **Built and tested on the PC:**
  - `ps4/gnmlib`: the GNM layer, 7 unit tests passing. PM4, GFX7 descriptors, colour / depth targets, .rsh parsing and the
    VS / PS register words, PS4 memory / driver / video-out wrappers.
  - The WGSL -> GCN pipeline: `ps4/tools/wgsl2spv` (naga), `ps4/tools/vkc` (radv in WSL, pipeline-binary export) and
    `ps4/tools/radvbin` (blob -> .rsh with checks). One command runs it all: `sh ps4/tools/shaders.sh`.
  - `ps4/tools/radvbin/layout.c`: the exact Mesa 26.1.6 struct offsets. `ps4/tools/vkc/layoutq.c`: radv's descriptor-set
    offsets.
- **Design:** `PS4_GPU_DESIGN.md` now has the radv resource ABI as observed, the shader cache, and the full wgpu-on-GNM
  back-end design. The plan: the vendored wgpu-hal's `noop` module becomes the GNM back end on the PS4 target, so wgpu and
  wgpu-core stay unmodified. It also covers the capture / compile / cache pipeline, the work order and sizes.
- **WSL Kali packages installed** (with the user's OK): `mesa-vulkan-drivers vulkan-tools glslang-tools spirv-tools
  libvulkan-dev gcc mesa-drm-shim`.

**Next: the GPU renderer, designed in `PS4_GPU_DESIGN.md`** (the census of the game's GS work, GS-full-on-GPU against porting
rac-rs's own renderer through a wgpu GNM back end, the recommendation and its three measurements). `ps4/probe/src/census.rs`
(PC, `PROBE_CENSUS=FROM:TO` -> `census.txt`) is part of that.

`ps4/gnm` (`sh ps4/build.sh gnm && sh ps4/pkg.sh gnm`, PKG `IV0000-RACR00004_00-RACRSGNMTEST0000`, log `/data/rac-rs/gnm.log`).
- Shaders: `ps4/gnm/shaders/tri_vs.ll`, `tri_ps.ll` (our own LLVM IR, committable). `build.sh` compiles each with
  `llc -mtriple=amdgcn-- -mcpu=gfx701` to `build-ps4/gnm/shaders/<name>.bin` (the `.text`) and `<name>.cfg` (llc's
  `.AMDGPU.config` pairs: RSRC1, RSRC2, SPI_PS_INPUT_ENA/ADDR), and passes the folder as `RAC_GNM_SHADERS` for `include_bytes!`.
  llc does not set USER_SGPR in RSRC2; the program sets it (VS: 1, the angle in user data 0 -> s0; PS: 0, so s0 = prim mask).
  The VS makes 6 vertices from the vertex id (background triangle + RGB triangle), no vertex buffers.
- One 26 MB direct-memory block (type 3, prot 0x33): 3 render targets that are also the video out buffers (linear, filled
  magenta first), 2 command buffers, shaders at 256-byte alignment, a label.
- Per frame: `sceGnmDrawInitDefaultHardwareState350` (256 dw), `sceGnmSetVsShader` (29 dw, regs: pgm lo/hi, rsrc1, rsrc2,
  spi_vs_out_config, spi_shader_pos_format, pa_cl_vs_out_cntl), `sceGnmSetPsShader350` (40 dw, regs: pgm lo/hi, rsrc1, rsrc2,
  z_format, col_format, ps_input_ena, ps_input_addr, ps_in_control, baryc_cntl, db_shader_control, cb_shader_mask; the order
  shadPS4's implementation reads), then our own SET_SH/CONTEXT/UCONFIG_REG packets: RSRC3 CU masks, user data, CB_COLOR0_*
  (tile mode index 8 = linear aligned, 8_8_8_8, COMP_SWAP ALT), target mask, blend off, depth off, scissors, viewport,
  VTE, AA mask, PS input cntl, primitive type; NUM_INSTANCES; `sceGnmDrawIndexAuto(6)`; EVENT_WRITE_EOP
  (CACHE_FLUSH_AND_INV_TS, 32-bit label = frame + 1). Submit with `sceGnmSubmitCommandBuffers` + `sceGnmSubmitDone`, CPU waits
  for the label (2 s timeout), then flips with `sceVideoOutSubmitFlip`. The log has each driver call's return value and three
  pixels read back after the first frames (magenta = the GPU wrote nothing).
- Untested guesses to check first if it fails: the RSRC3 CU masks (0xffff), tile mode index 8, COMP_SWAP, and whether the
  driver wants anything else in the command buffer before a submit.

Previously planned (before this result): shaders compile -> design the GPU GS (GS-full's raster stage as GL draws, PS2 VRAM emulation like PCSX2's
hardware renderer); no compiler -> look for a way to get PSSL binaries (system compiler elsewhere, or a cache) or GNM.

The goal is to play R&C lying down on a jailbroken PS4 Pro, with our built-in mods except ray tracing (Ragdoll Ratchet,
Hoverboard, Slam Shockwave). PS2 Classics FPKGs were ruled out because R&C1 freezes there. Streaming was set aside. New mods
can later ship as GoldHEN update PKGs (patch/DLC style) if recompiled code and our code stay separable, and mod settings live
in a writable file. The PS4 has no launcher, so upstream's in-game Mods page would come back on PS4.

**Never share the probe PKG or anything in `build-ps4/`:** it contains code recompiled from the user's disc.

## How the PS4 build works

- **Target:** `ps4/x86_64-rac-ps4.json` is a custom Rust target.
  - `os = freebsd`, `llvm-target x86_64-pc-freebsd12-elf`, `cpu btver2` (Jaguar: SSE4.1/AVX, no AVX2/FMA/BMI2).
  - PIE, `relro-level partial` (create-fself needs a PT_GNU_RELRO segment), `has-thread-local false`.
  - `panic = unwind`, which the rec code's catch_unwind needs.
- **Building std:** build-std uses `RUSTC_BOOTSTRAP=1`, `-Z build-std=std,panic_unwind` and `-Z json-target-spec`, from a
  patched copy of rust-src.
  - The copy lives in `build-ps4/std-src` and is made once from `ps4/std.patch`. Delete it to re-apply the patch.
  - Cargo is pointed at it with `__CARGO_TESTS_ONLY_SRC_ROOT`.
  - The patch puts libc in FreeBSD 11 mode (`--cfg libc_unstable_freebsd_version="11"`: 32-bit ino/dev, 16-bit nlink), so it
    fixes the one std line (`d_ino`) that mode breaks.
  - It also strips the `@FBSD_1.0` symbol versions from the vendored libc 0.2.189, wired in via `[patch.crates-io]` in
    `library/Cargo.toml`.
- **Linking (`ps4/build.sh`):** rust-lld (gnu flavor) with OpenOrbis's `link.x` and `crt1.o`.
  - `-lkernel` goes **first**, so every pthread and file call binds to libkernel's FreeBSD ABI. OpenOrbis musl `libc.a` fills
    in the rest (malloc, strings).
  - Also linked: `-lSceNet` (std calls `sceNetGetDnsInfo`) and `-lSceSystemService` (for quitting).
  - Empty stub archives stand in for gcc_s, execinfo, pthread (to shadow musl's), kvm, procstat, memstat and devstat.
  - **No `-lunwind`**: see the shim.
- **Packaging:** `create-fself` (needs `OO_PS4_TOOLCHAIN`) turns the ELF into `eboot.bin`. `ps4/pkg.sh` builds param.sfo
  with PkgTool.Core (`DOTNET_ROLL_FORWARD=Major`), runs create-gp4, and builds the fake PKG.
  - hello: `IV0000-RACR00001_00-RACRSHELLO000000`
  - probe: `IV0000-RACR00002_00-RACRSPROBE000000` (carries `oracle/stubs.txt` as `/app0/stubs.txt`)
  - The version stays 01.00, so the user must delete the old app before installing a new build.
- **Toolchain:** OpenOrbis v0.5.4 `toolchain-llvm-18.tar.gz`, extracted to `toolchain/openorbis/OpenOrbis/PS4Toolchain`.
  rustup components rust-src and llvm-tools were added to the private toolchain. There is no clang on this machine.

Commands (Git Bash, from the game folder, after a completed setup):

```bash
sh ps4/build.sh hello && sh ps4/pkg.sh hello
sh ps4/build.sh probe && sh ps4/pkg.sh probe
sh ps4/prof.sh /path/to/profile.txt
```

The probe also builds for Windows, which is the way to check the probe itself. Run it with
`build-ps4/release/rac-ps4-probe.exe ISO STUBS LOG FIELDS STACK_MB` plus the env vars `PROBE_CARD`, `PROBE_SCRIPT` and
`PROBE_FRONT=1`. Build it with the same cargo line minus the PS4 target. `sh ps4/prof.sh profile.txt build-ps4/release/rac-ps4-probe.exe`
symbolizes a PC profile.

## Where the work lives

Branch `ps4-port` in this folder (cut from `mods-migration`), pushed to its own private repo
https://github.com/TheZappster99/rac-rs-ps4 (remote `ps4`, its `main`). A worktree would not work: the toolchain and the
disc-generated code exist only in this folder. Mods work stays on `mods-migration`; merge it into `ps4-port` to bring mods
over. Never commit `build-ps4/`.

## Files

- **`ps4/`**: everything listed above, plus `README.md` (the player's test steps and developer notes).
  - `shim/` (crate `rac-ps4-shim`, PS4-only, empty elsewhere):
    - `arc4random_buf` (rdtsc + splitmix64).
    - `pthread_setname_np`, which wraps libkernel's `pthread_set_name_np`.
    - The **`unwinding` crate** as the unwinder. `init()` must be called first in `main`: it points the crate at OpenOrbis's
      `__text_start` / `__eh_frame_start` / `__eh_frame_hdr_start`.
    - `quit(code)`: `sceSystemServiceLoadExec("exit")`, then `_exit`.
  - `hello/`: std smoke test. It writes `/data/rac-rs/hello.log`.
  - `app/`: the real game (rac_game + ee-game) on the GNM back end, PC (fake GPU) and PS4 (step 5 above): `src/main.rs`
    (PS4 start-up), `src/heap.rs` (direct-memory heap), `seed/` (first-start settings).
- **PS4 gating in shared rac-rs files** (all `cfg(target_os = "freebsd")`, PC unchanged; keep in mind when merging upstream):
  - `rac-rs/Cargo.toml`: cpal and the full wgpu are PC-only; the PS4 gets wgpu with `std, wgsl, noop, parking_lot`.
  - `audio.rs`: cpal stand-in types, `Audio::new` = None. `viewer/gpu/reshade/native.rs`: no Vulkan path.
  - `viewer.rs` (winit imports vs `ps4win`), new `viewer/ps4win.rs`, `viewer/handler/cursor_vis.rs`, `viewer/pace.rs`
    (`&super::Window`), `runlog.rs` (`set_window` for the stand-in).
  - `lib.rs` (`set_args`) + `main.rs` (takes those args when there are none).
  - `retail/worker/spawn.rs` (worker stack 16 MB on the PS4), `viewer/startup/bgpool.rs` (stacks capped at 8 MB on the PS4).
  - `ps4/shim/lib.rs`: the posix_spawn stubs.
  - `probe/`: the headless core.
    - `main.rs`: cold boot, memory card load, pad script, movie-skip hands, unwind every 10 fields, timing split, level
      speed line.
    - `prof.rs`: the PS4 SIGPROF sampler.
    - `prof_win.rs`: the PC twin, which suspends the thread named "ee".
    - `gsf.rs`, `standins.rs`, `vout.rs`: the picture (step 2, part 1).
- **`rac-rs/vendor/wgpu-hal`** (upstream's vendored copy): `src/ps4key.rs` (pipeline key), `src/vulkan/ps4cap.rs`
  (capture, `RAC_PS4_CAPTURE`), and small hooks in `src/vulkan/device.rs` (marked "rac-rs PS4 port"), plus one line each in
  `src/lib.rs` and `src/vulkan/mod.rs`. They do nothing unless the variable is set. Port them by hand if an upstream release
  replaces the vendored hal.
- **`ps4/gnmlib/src/cache.rs`**, `ps4/tools/vkc/vkrec.c`, `ps4/tools/shcache/`, `ps4/tools/shcache.sh`: the shader
  cache (PS4_GPU_DESIGN.md, work order step 3).
- **`oracle/src/lib.rs`**: the game thread's stack size reads `ORACLE_EE_STACK_MB` (default 256, so the PC is unchanged).
  This is a small diff to an upstream file; keep it in mind when merging upstream.
- **`.gitignore`**: adds `build-ps4/`.
- `Cargo.lock` was already modified before this work, and `mod_backups/` was already untracked. Neither is part of it.
- Memory note: `ps4-port.md` in the auto-memory.

## What was learned (on the console)

- **Memory:** about 280 MB of flexible memory (malloc, mmap, thread stacks), and 4.6 GB of direct memory, which is unused
  so far. A 256 MB thread stack fails with EAGAIN next to the 166 MB eboot, so the probe uses 32 MB.
  - The stack only needs to be big because compiled code nests a Rust frame per retail call, and some paths never return.
    The host must set `m.cpu.rec_unwind = true` every 10 fields, as rac-rs `retail/worker.rs` `UNWIND_FIELDS` does.
    Without it the stack grows ~160 KB a field, and even 256 MB overflows after ~1400 fields.
- **OpenOrbis `libunwind.a` is built without optimization** (one call per byte read): ~9 µs per frame against 0.1 µs on the
  PC, and 12% of level time. Replacing it with the `unwinding` crate halved the per-frame cost and took the unwinder down to
  2.4% of time.
- **Returning from `main` crashes** (CE-34878-0). Quit with `rac_ps4_shim::quit`.
- **Signal context layout:** in the SIGPROF handler's third argument, rip is word 28 and rsp is word 31. That is the FreeBSD
  order, six words later than stock FreeBSD's `ucontext_t`.
- **Speed (USA disc, NTSC 59.94):**

  | Part of the run | PC | PS4 |
  |---|---|---|
  | Boot | 263% | 85% |
  | Menus (mostly interpreted PS2 code) | ~205% | 25% |
  | Level 7, walking | ~580% | 98-99% (16.2 ms per field against a 16.7 ms budget) |
  | Just after the level loads (fields ~1400-1800) | | 38-66% (not investigated) |

  The level profile matches the PC's: `skin_ff` 16%, `vu0_prog0_run` 5%, `rec::Table::stale` 3%, hs_hands regions, rec
  code. The PS4 core is simply ~6-7x slower than this PC, and only one of its 7 usable cores is used.
- **Pad script that works (USA, from a cold boot):** `220:start,280:down,320:x,420:x`. Start opens the title menu (front mode
  word USA 0x15f604 = 3). Down and Cross pick Load Game. Cross loads the first save at ~430. The level is playable from
  ~1160. Pad bits: Cross 0x40, Circle 0x20, Triangle 0x10, Square 0x80, Start 0x800, Up 0x1000, Right 0x2000, Down 0x4000,
  Left 0x8000, L2 1, R2 2, L1 4, R1 8.

## Next steps

1. **Picture (step 2).** The PC renderer is wgpu on Vulkan, which the PS4 lacks. The renderer needs a PS4 backend on GNM/GNMX
   (or OpenOrbis's graphics wrappers / piglet GLES). Start with a test that gets one frame on the TV:
   - Video out first (`sceVideoOut*` with a framebuffer), even CPU-filled.
   - Then decide between porting the wgpu-side renderer (`rac-rs/src/viewer/`, `gsfull/`) to a PS4 GPU API, or drawing the
     oracle's own GS output (`gscap` / `gs::Gs`) first as the simplest path to a picture.
   - Ray tracing stays PC-only. Ragdoll's renderer part needs porting; Hoverboard and Slam Shockwave should port as they are.
2. **Sound and input:** built in step 6 (`sceAudioOut`, `scePad`), waiting for the console test.
3. **Speed headroom:** use other cores. `skin_ff` and VU0 work are the candidates; on the PC, GS/VU1 and sound already run off
   the EE thread. Also look at the post-load slow stretch and PGO for the PS4 build.
4. **Productizing:**
   - Settings and mods from a writable file in `/data/rac-rs`, with upstream's in-game Mods page re-enabled on PS4: done
     (2026-11-09, "Saving, options and mods"), waiting for round 8 runs 6-8.
   - Update PKGs for new mods.
   - Save card written to `/data/rac-rs`, synced: done (same section), waiting for round 8 run 6.
