# RAC-RS on PS4: step 2, a picture

This folder is an early experiment: running RAC-RS on a jailbroken PS4 (GoldHEN). The game's core runs on the console
(step 1), and the probe now also shows the game's own picture on the TV (step 2). There is no sound and no controller yet:
the probe presses buttons by itself to get through the menus and load your first save.

| Program | What it does | Package |
|---|---|---|
| `hello` | Checks that Rust programs work on the PS4: threads, memory, files, error recovery. | `build-ps4/hello/IV0000-RACR00001_00-RACRSHELLO000000.pkg` |
| `probe` | Starts Ratchet & Clank from your disc image, shows the picture, presses buttons to get into your first save, and logs how fast everything runs. | `build-ps4/probe/IV0000-RACR00002_00-RACRSPROBE000000.pkg` |
| `app` | The real game: draws with the PS4 graphics chip, presses buttons by itself to load your first save. No sound or controller. | `build-ps4/app/IV0000-RACR00007_00-RACRSAPP00000000.pkg` |
| `app` (pad, sound) | The same game with the controller and sound added. Installs next to the one above. | `build-ps4/app/IV0000-RACR00008_00-RACRSAPP00000002.pkg` |

The probe's package contains code made from your own disc. It is for your own console only: never share it.

## Running the probe

1. **Delete the old "RAC-RS PS4 core probe" from the home screen first** (the version number does not change, so the new
   package will not install over it).
2. Install the new `.pkg` (USB stick, or GoldHEN: Debug Settings > Package Installer).
3. `/data/rac-rs` should still hold `game.iso` and `card.bin` from step 1 (copy them over FTP, port 2121, if not).
4. Start **RAC-RS PS4 core probe**. You should see:
   - **colour bars** at once (the PS4's video output works), then
   - **the game's picture**, in 4:3 with black side bars. It will be slow, probably a few pictures a second: the whole PS2
     picture is drawn by the CPU for now, so this test is about seeing that it is right, not about speed.
   It stops after 3000 game fields. That takes a few minutes at this speed; you can also quit with the PS button at any time.
5. Copy back from `/data/rac-rs`: `probe.log`, `profile.txt` and any `frame_01000.ppm`, `frame_02000.ppm`, ... (snapshots of
   the picture). Tell me what the TV showed: bars, a picture, garbage, or black.

**The current probe build measures instead of drawing:** it runs the PS2's vector unit work for the picture but draws nothing,
so the TV shows only colour bars while the game runs at full speed in the background. Put a file `gs_mode.txt` containing
`full` in `/data/rac-rs` to get the (slow) picture back.

Optional: `gs_threads.txt` in `/data/rac-rs` containing a number (e.g. `4`) sets how many CPU threads draw the picture (default 3).

If the probe crashes, its log stops at the last step that worked, which is just as useful.

## Running the GPU test

The probe's picture is drawn by the CPU, which is too slow on the PS4. This small test checks whether we can draw with the
PS4's graphics chip instead. It does not need the disc image.

1. Install `build-ps4/gpu/IV0000-RACR00003_00-RACRSGPUTEST0000.pkg` and start **RAC-RS PS4 GPU test**.
2. You should see either lots of small coloured triangles over a purple and checkered background for about half a minute,
   or a few seconds of slowly changing colour (that means the drawing part did not work, but the screen did). It then goes
   back to the home screen.
3. Copy `/data/rac-rs/gpu.log` back to the PC and tell me what you saw.

## Running the GNM triangle test

The GPU test above showed that the system's OpenGL library cannot be used, so this test draws with the PS4's own graphics
driver, the way PS4 games do. It does not need the disc image.

1. Install `build-ps4/gnm/IV0000-RACR00004_00-RACRSGNMTEST0000.pkg` and start **RAC-RS PS4 GNM triangle**.
2. If it works you see a spinning red, green and blue triangle on a dark blue background for about ten seconds, then it goes
   back to the home screen. Magenta, black or a frozen picture also tells me something.
3. Copy `/data/rac-rs/gnm.log` back to the PC and tell me what you saw.

## Running GNM test 2

This draws with shaders written the same way as the PC game's (WGSL), compiled for the PS4's graphics chip on the PC. It does
not need the disc image.

1. Install `build-ps4/gnmtest/IV0000-RACR00005_00-RACRSGNMTEST0002.pkg` and start **RAC-RS PS4 GNM test 2**.
2. If everything works: a spinning cube with coloured faces in front of a checkered, bluish background for about ten seconds.
   Anything else (magenta, black, an error, a frozen screen) is just as useful to know.
3. Copy `/data/rac-rs/gnmtest.log` back to the PC and tell me what you saw.

## Running the game (first try)

This is the real PC game (rac-rs), drawn with the PS4's graphics chip. There is no sound and no controller yet: it presses
buttons by itself to load your first save, then walks Ratchet forward for a while.

1. Install `build-ps4/app/IV0000-RACR00007_00-RACRSAPP00000000.pkg` and start **RAC-RS PS4**.
2. `/data/rac-rs` needs `game.iso` (your disc image) and `card.bin` (your save card), as for the probe. On the first start the
   game also writes its settings there (`pc_options.txt`, `mods/`).
3. Watch the TV for about two minutes: logos, the title screen, the save loading, Veldin. Then quit with the PS button.
4. Copy back from `/data/rac-rs`: `log.txt` (and `log.prev.txt` if you started it twice), `missing_pipelines.txt` if there
   is one. Tell me what the TV showed.

Optional: a file `env.txt` in `/data/rac-rs` with `NAME=value` lines changes settings without a new package, for example
`RAC_PS4_FRAMES=3000` (stop by itself after 3000 frames) or `RAC_RETAIL_PAD=` (no button presses at all).

## Building (developers)

From Git Bash in the game folder, after a completed setup:

```bash
sh ps4/build.sh hello && sh ps4/pkg.sh hello
sh ps4/build.sh probe && sh ps4/pkg.sh probe
sh ps4/build.sh app && sh ps4/pkg.sh app
```

`build.sh` needs the OpenOrbis toolchain in `toolchain/openorbis` (release v0.5.4, `toolchain-llvm-18.tar.gz`) and builds
Rust's standard library for the PS4 from a patched copy (`std.patch`). How it works:

- `x86_64-rac-ps4.json` is the Rust target: FreeBSD (the PS4's system is a FreeBSD 9 fork) with the Jaguar CPU (`btver2`),
  linked by `rust-lld` with OpenOrbis's `link.x` and `crt1.o`.
- The PS4's `libkernel` provides the FreeBSD calls (threads, files, locks) and is linked first; OpenOrbis's musl `libc.a` fills
  in the rest (memory allocation, strings). std is built in FreeBSD 11 mode (`libc_unstable_freebsd_version="11"`), which matches
  the PS4's file and directory layouts.
- `shim/` supplies the two C functions std wants that the PS4 lacks (`arc4random_buf`, `pthread_setname_np`).
- The picture: `probe/src/gsf.rs` runs rac-rs's GS-full pipeline (`rac-rs/src/gsfull`, compiled in unchanged through a
  `#[path]` module, with `probe/src/standins.rs` for the two rac-rs modules it touches) and `probe/src/vout.rs` shows it with
  `libSceVideoOut` (CPU-filled 1080p buffers in direct memory).
- `create-fself` turns the ELF into `eboot.bin`; `pkg.sh` builds `param.sfo` and the fake PKG with OpenOrbis's `PkgTool`.
