# rac-rs PS4 port (experiment, stopped)

An experiment to run **[rac-rs](https://github.com/AsobiCodes/rac-rs)**, the fan-made PC port of Ratchet & Clank (PS2, 2002) by
AsobiCodes, on a jailbroken PS4 (GoldHEN). This is not an official rac-rs project, and it is not supported.

**Status: stopped. This project is very unlikely to continue.** The code is shared as it was left, in case anyone wants to pick
it up or learn from it.

## Where it got to

- The real game builds for the PS4 with a custom Rust target and the OpenOrbis toolchain, and installs as a package (PKG).
- It boots from your own disc image, shows the menus and draws levels on the PS4's graphics chip through a new graphics back end
  written for this port (`rac-rs/vendor/wgpu-hal/src/gnm`).
- It was tested on a PS4 Pro over many rounds. With the heaviest graphics effects switched off it ran at roughly 20 frames a second.

## Why it stopped

rac-rs reproduces the PS2's processor exactly, so the gameplay stays identical to the original. That exact work is too heavy for
the PS4's processor:

- Full speed needs the game's own work done in 16.7 ms a step (60 a second).
- On the PS4 Pro it took about 20 ms in quiet areas and about 41 ms in busy ones.
- After measuring where the time goes, the best estimate with every known fix (moving character skinning to other cores, tuning the
  recompiled code) is 26 to 30 ms in busy areas. That is roughly 55 to 65% speed, and the game would run in slow motion there.

Getting much further would need a different approach, such as a full decompilation of the game rebuilt as ordinary code. That is
far beyond this experiment.

## Where to look

- `PS4_PORT_PLAN.md`: the full hand-off. It has how to build, every console test round, what worked, the console gotchas and the
  speed measurements that led to stopping.
- `PS4_GPU_DESIGN.md`: how the PS4 graphics back end works.
- `ps4/`: the PS4 target, build and package scripts, the GNM layer, tools and the console test setups.
- `MODS-README.md`: the PC mods this branch also carries (ray tracing, ragdoll and more).

## What is not included

Nothing from the game disc is included. Building needs your own copy of the game (PAL SCES-50916 or USA SCUS-97199), from which
setup generates the game code locally. Never share generated code, built packages or anything made from the disc.

## License

GPL-3.0-or-later, like rac-rs. See `LICENSE` and `NOTICE.md`.
