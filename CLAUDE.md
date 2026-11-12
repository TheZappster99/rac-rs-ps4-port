# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

rac-rs is an unofficial Windows PC port of Ratchet & Clank (PS2, 2002) in Rust. It is a **static recompilation**: setup
translates the PS2 EE code from the user's own disc image into native Rust crates, which run on a PS2 CPU core written in
Rust. Everything around the game logic (wgpu renderer, audio, FMV, input, saves, PC graphics options) is native code in
`rac-rs/`. Supported discs: PAL v1.00 (SCES-50916) and USA v1.00 (SCUS-97199). Windows only.

**Never commit or share generated code or anything derived from disc bytes.** `.gitignore` lists the generated paths
(`oracle/rec/`, `oracle/game/`, `oracle/src/vu0_prog*.rs`, `oracle/src/skinff_*.rs`, `rac-rs/src/engine/gsfull/vu1_prog*.rs`,
`vu1aot_gen.rs`, `rac-rs/src/rac1/particles/orb_tables.rs`, plus `build/`, `generated/`, `player/`, `toolchain/`, `*.iso`).

## Building and running

The project uses its own private toolchain in `toolchain/` (Rust 1.99.0, host `x86_64-pc-windows-gnu`, plus Python and GNU
as), installed by `setup.ps1`. To call cargo by hand, mirror what setup does (PowerShell):

```powershell
$env:RUSTUP_HOME = "$PWD\toolchain\rustup"; $env:CARGO_HOME = "$PWD\toolchain\cargo"
$env:RUSTUP_TOOLCHAIN = "1.99.0-x86_64-pc-windows-gnu"; $env:Path = "$PWD\toolchain\cargo\bin;" + $env:Path
cargo build --release --manifest-path rac-rs\Cargo.toml --target-dir build -p rac-rs-exe --bin rac-rs --config generated\build_profile.toml
```

- If the build fails with `dlltool.exe: program not found`, also put
  `toolchain\rustup\toolchains\1.99.0-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained` on `Path`.
- Setup's real build also adds `--features rac-rs-exe/windows_gui` (no console window; panics go to `player/crash.log`) and a
  PGO `--config` built from `pgo/<serial>.profdata.gz`. Omit `windows_gui` for dev builds so stderr is visible.
- The generated code must exist first (`Setup.cmd`, or `Setup.cmd -Console` for console output; "Repair / rebuild" regenerates).
  Generators: `tools/recgen_iso.py` (EE code -> `oracle/rec/*` + `oracle/game`) and `tools/vugen_iso.py` (VU microcode).
- Run: `Play.cmd` (written by setup) sets `ORACLE_ISO`, `ORACLE_STUBS`, `ORACLE_HAND_SKIP`, `RAC_PLAYER_DIR`, `COLD_LANG` and runs
  `build\release\rac-rs.exe view <iso> 0 --retail-cold --modern-cam`. Extra args pass through. Other subcommands (`levels`,
  `terrain`, `ties`, `scan`, `dump`, `warm-gpu`, `view --shot out.png --cam ...`) are listed in `USAGE` in `rac-rs/src/main.rs`.
- The run log is `player/log.txt` (previous run: `player/log.prev.txt`).

### Tests

Since v0.5.0 the tests live in each crate. `rac-engine`'s (renderer, shaders, omni, plugins.rs) do not link the generated code
and run fine (~1300 tests, 4 min); `rac1`'s link `ee-game` (a dev-dependency) and need a completed setup; `rac-rs`'s are the
source-tree checks (size limits, boundaries, env docs).

```powershell
cargo test --manifest-path rac-rs\Cargo.toml --target-dir build -p rac-engine --lib <test_name_filter>
```

Known failures in this fork: `rac-rs` `size_limits_tests::no_file_or_fn_grows_past_its_limit` (our mod hooks grow upstream fns such
as `frame_hero`, `pass_world`, gpu `setup::new`) and the two `env_doc_tests` (they need `rac-rs/ENV.md`, which the release does
not ship). Many tests read env vars such as `RAC_TEST_STATE` / `RAC_TEST_FIELDS`.

## Architecture

- **Workspace** (`Cargo.toml` at the root, members `rac-rs` and `rac-rs/exe`). It sits above `oracle/` so generated crates are
  inside it and symbol names are path-stable across installs, which the shipped PGO profiles depend on.
- **Three crates since v0.5.0** (rac-rs/crates/*/Cargo.toml, sources in rac-rs/src/*): `rac-formats` (`src/formats`: disc and
  file formats), `rac-engine` (`src/engine`: renderer `viewer`, `omni`, audio, fmv, gsfull, pcopts, our `plugins.rs`) and `rac1`
  (`src/rac1`: `retail`, `host` = the window app / handler / `App` struct, frontend, the old hand port). rac1 may use engine;
  engine may not use rac1 (pass data in, or keep a hook in rac1). Each crate re-imports the others' modules under their old
  `crate::` names (rac1 `lib.rs` `use rac_engine::{...}`), so a new engine module rac1 reads must be added to that list.
- **`rac-rs/` (crate `rac_game`, lib)**: the command line and the source-tree tests. `src/lib.rs` does `include!("main.rs")` and declares
  `extern crate self as ee_game`; the real `ee_game::register` is injected at runtime by the exe via `set_register`. This lets
  the ~460 generated crates and `rac_game` compile in parallel instead of serially. Keep it that way: `rac_game` must not depend
  on `ee-game` except as a dev-dependency.
- **`rac-rs/exe/` (package `rac-rs-exe`, bin `rac-rs`)**: thin `main` linking `rac_game` + `ee-game`. `order_a`/`order_b` are
  empty crates that only influence cargo's scheduling order.
- **`oracle/` (crate `ee-oracle`)**: the PS2 side: EE CPU (`cpu.rs`, `oracle/core` = `ee-core`), memory, VIF/VU, GS, SPU2, 989snd,
  memory card, HLE, cold boot from disc (`cold.rs`), version detection (`ver.rs`). `oracle/stubs.txt` lists retail functions
  replaced by fixed returns (IOP is not emulated). `oracle/rec/<level>_<nn>` and `oracle/game` are generated.
- **"Hands"**: original retail functions rewritten in readable Rust inside `rac-rs` (identified by PAL entry address + hash).
  `rac-rs/src/rac1/retail/disc_hands.rs` maps them to USA addresses (generated by `tools/gen_hands_usa.py`; edit the generator, not
  the file). `oracle/hand/hand_skip.txt` lists hands to skip. Behaviour changes are validated by running the same frames on
  retail and rewritten code and diffing game memory; gameplay must stay 1:1 with the PS2.
- **`rac-rs/src/rac1/retail/`**: drives the recompiled game (feeding pad input per tick, capturing its GS output in `gscap`, memcard,
  key binds, PC menu extensions `pcext.rs`/`pcopts.rs`, debug keys).
- **`rac-rs/src/engine/viewer.rs` + `viewer/`**: the wgpu (30.x, Vulkan by default) renderer (`gpu.rs`, `gpu/draw/*`: the
  post passes are `pass_post.rs`); the window/app loop is rac1 `host/` (`app.rs`, `handler.rs`, `startup.rs`). Large shared WGSL lives as string constants in `viewer.rs` (`SHADER`); all sun-shadow receivers go
  through the WGSL `sun_occ(p)`. Enhanced effects are in sibling modules (`sunshadow.rs`, `waterfx.rs`, `vegfx.rs`, `post.rs`,
  `smooth.rs` frame interpolation, `hudsharp.rs`, ...).
- **`rac-rs/src/engine/gsfull/`**: full GS/VU1 path (VU1 microcode translated to Rust, ahead-of-time).
- Level-asset decoding (`wad.rs`, `iso.rs`, `level.rs`, `terrain.rs`, `ties.rs`, `shrubs.rs`, `mobies.rs`) reads the disc at runtime.

## Built-in mods (plugins)

Optional PC features compiled into the game (no DLLs, no scripts) that the player switches On / Off like any other mod: in the
RAC-RS Launcher's Mods page only (upstream's in-game Mods pages, `retail/pcmods.rs`, are switched off by `pcext::MODS_PAGE`). A built-in mod is a
Rust type implementing `plugins::Plugin` (`rac-rs/src/engine/plugins.rs`) plus one entry in the `PLUGINS` list there. Its code may live
anywhere (renderer in rac-engine, retail driver in rac1); it only asks `crate::plugins` whether it is on. A game-side mod's per-field
work is called from rac1 `retail.rs` `builtin_pre_field` (the engine cannot call into rac1); its drawing lives in rac1 `host/`
(`host/ragdoll.rs`, `host/hoverboard.rs`) when it reads retail state. New optional features should be built-in mods
rather than new env vars or new Options rows. Do not add menu pages of our own: upstream's Mods page owns `BASE+0xC000..0xD800` and
text ids `0x7fc0..0x8000` (upstream's Display page `pcdisp.rs` owns `BASE+0xE000..0xE400` and text ids `0x7fb0..0x7fc0`). We own no RAM range or text id now: our old title Quit Game row (`titlequit.rs`) was dropped at v0.5.0,
which has its own Quit to Desktop.

- **How it plugs into the mods system**: on first use `plugins.rs` writes `mods/<id>/omni-mod.json` for each built-in mod (format 2,
  `"type": "script"` because the launcher accepts only script / native, no scripts, `"builtin": true`; rewritten only when it
  differs). `omni/load.rs` `start` drops built-in ids before planning, so OmniLoader never sees them (and is not even loaded when
  only built-in mods are on).
- **On / Off**: the mod's id line in `mods/active.txt` under the "Load mods" master (`omni::plugins::active_ids`; no line = off).
  **Settings**: `mods/settings.json` under the mod's id, stored by value name (lower case, spaces as `-`). Both are read once at
  start-up (`plugins::on_at_start("id")`, `value_at_start("id", "setting")` -> index into `values`): mods change at the next start.
  rac1 `retail::builtin_pre_field(&mut Machine)` runs before each level field for the mods that are on (after modcam/strafe/aim).
- **Settings shape**: `fn settings() -> &'static [Setting]` (`id`, `name`, 2..=4 `values`, `default` index), written into the
  manifest as `choice` settings, so the launcher shows them.
- **Rules** (checked by `plugin_ids_and_names_fit_the_file_and_the_menu`): ids are the loader's mod ids (lower case letters and
  digits joined by single hyphens, e.g. `ray-tracing`); setting ids `[a-z0-9_]`; names and values are ASCII letters, digits,
  spaces and `-` only (the game's font), names <= 24 chars, values <= 16. Every mod has a `description()` for the launcher.
- `env_force()` names a `RAC_...` var that forces the mod on when `=1` (keeps old dev switches working, e.g. `RAC_RAGDOLL`). A part of a mod can keep its own switch too: the Ray tracing parts read `RAC_RT_SHADOWS`, `RAC_RT_RAYS`, `RAC_RT_REFLECTIONS` (`rtshadow::part`).

## Dev env vars

- `RAC_WGSL_DIR=DIR`: each shader module loads `DIR/<label>.wgsl` if present, otherwise dumps its built-in source there
  (`rac-rs/src/engine/devtune.rs`). Handy for live shader experiments without rebuilding.
- Many feature/debug switches are read with `std::env::var("RAC_...")` (e.g. `RAC_PERF_LOG`, `RAC_VSYNC`, `RAC_VEG_*`,
  `RAC_WATER_*`); grep for the name before adding a new one.

## In-progress work

`RT_SHADOWS_PLAN.md` describes the ray-traced sun shadows (`rac-rs/src/engine/viewer/rtshadow.rs`, enabled with
the Shadows setting of the "Ray tracing" built-in mod (`ray-tracing`, which holds every ray-traced effect as a setting: Shadows, Sun rays, Reflections) or `RAC_RT_SHADOWS=1`, Vulkan + `EXPERIMENTAL_RAY_QUERY` only). Read it before touching shadows. `build\release\rac-rs.orig.exe`
is the untouched exe kept for A/B comparison.

`RT_REFLECTIONS_PLAN.md` describes the ray-traced reflections (`rac-rs/src/engine/viewer/rtrefl.rs`, the Reflections setting of the same mod,
with a quality and a GPU time budget each). Its status line says which phase is next.

`RAGDOLL_PLAN.md` describes the Ragdoll Ratchet built-in mod (`ragdoll-ratchet`: `rac1/retail/ragdoll.rs` worker, `rac1/host/ragdoll.rs`
renderer, forced on with `RAC_RAGDOLL=1`). Its "Progress so far" section is the hand-off: what works, the approved tuning, what is next.

`PS4_PORT_PLAN.md` describes the PS4 port experiment (`ps4/`: custom FreeBSD-based Rust target, OpenOrbis toolchain, headless
core probe that runs a level at ~99% of PS2 speed on the user's PS4 Pro). Read it before any PS4 work: it lists the build steps,
the console gotchas and the next step (the picture). `build-ps4/` holds disc-derived code and must never be shared.

## Updating from upstream

This folder (`F:\Games\Ratchet and Clank Remastered`, the RAC-RS Launcher's install) is the dev copy: branch `mods-migration` = our
work on top of the official releases, `main` = the official releases only, one "Upstream rac-rs vX as downloaded" commit each. Never
let the launcher update this install (it would replace our code). A new official release is installed into a separate folder, then:

1. Commit that folder's files onto `main` without touching this working tree: a temporary `GIT_INDEX_FILE`, `git read-tree main`,
   `git --work-tree=<new folder> add -A`, `git write-tree`, `git commit-tree -p main`. Take the tree before its setup has run (or
   drop the `Cargo.lock` setup rewrote and any `__pycache__`), and check no generated / disc-derived path slipped in.
2. `git merge main` into `mods-migration`; upstream often moves code between files, so port our hunks by hand where a file was split.
   Check upstream's new reserved RAM ranges / text ids against ours and the level shader's group-0 bindings (upstream's Metal look `metalenv.rs` uses 4..9, `rtshadow.rs` uses 10..14).
   After the merge grep every WGSL hook string our rt*.rs patch in (`replacen` + `assert!`) and run the `-p rac-engine` tests `rt`.
   If upstream replaced `rac-rs/vendor/wgpu-hal`, re-add the PS4 pipeline capture (`ps4key.rs`, `vulkan/ps4cap.rs`, hooks marked
   "rac-rs PS4 port" in `vulkan/device.rs`; see `PS4_PORT_PLAN.md`, Files).
3. Rebuild with `setup.ps1 -Console -Iso "<iso>" -NoShortcut` (regenerates only what changed) and run the game once.
4. Then `git merge mods-migration` into `ps4-port` and follow `PS4_PORT_PLAN.md` "Upstream v0.5.0": PS4 target build (new PC-only
   code needs freebsd fences), shader cache re-capture (`build-ps4/capture-v05.sh`, including one `RAC_BINDLESS=0` run), fake run
   (`sh ps4/pc-prof.sh`), PKG, shadPS4, restage the console round.

## Conventions

- Rust source is indented with tabs; comments are plain, explanatory prose stating *why*. User-facing text (README, CHANGELOG)
  is written in simple, non-technical English for players.
- `setup.ps1` rebuilds by file time and only rewrites changed generated files; avoid touching generated files needlessly.
