# Tests for when you are back (2026-11-08, PS4 part updated after round 2)

Do them in this order. Each one says what to do, what to look for, and which files to copy back. If something crashes or
freezes, the log still tells me what happened, so copy it back anyway.

FTP: the PS4's `/data/rac-rs` folder (GoldHEN FTP, port 2121). It should still hold `game.iso` and `card.bin`.

---

## PS4 test 1 result (2026-11-08)

Last night's build showed the level for about a second, very slowly, without the ground's texture, then froze the whole console
(power button needed). `log.txt` came back empty: the console had not written it out yet when it froze. `missing_pipelines.txt`
listed only `hud sharp` (2 pipelines). Test 2 (controller and sound) waits until the picture is stable.

## PS4 test round 2 result (diagnostics build)

Three pictures came back: frames 30 and 100 show the start-up level scene with buildings and rocks textured but the ground and
sky black (the same picture both times), frame 300 is all black, then the console froze before frame 1000. The log was empty
again: the PS4 does not let the app point its output at a file the way the PC does.

## PS4 test round 3 result

The log works now. The game runs at about 7 frames a second (the processor is the limit, not the graphics chip), reaches the
title menu, and the graphics chip hangs when your save starts loading its level; the app then closed itself. The pictures
showed that the black ground and shapes are really drawn, just in black.

## PS4 test round 4: everything is in `build-ps4	o-console`

Follow `STEPS.txt` there as before. This round hides one group of objects per picture to find which ones draw black, and if the
graphics chip hangs again the log names the graphics programs that ran for the first time just before.

---

## PC test 3: Hoverboard tricks (new tonight)

Start the game with **`Play-hoverboard-tricks.cmd`** (in the game folder). It uses your normal saves and mod settings, but runs
tonight's tricks build. Turn the Hoverboard mod on in the launcher first; the new settings Tricks and Bails are On by default. A
controller is best.

The basic board riding was never tested either, so check that first: if riding itself is broken, stop there and tell me.

Quick version (the full list, with the log lines to look for, is at the end of `HOVERBOARD_TRICKS_PLAN.md` in
`.claude\worktrees\hoverboard-tricks-0de9a2`):

1. Ride with Tricks Off: it should ride as before. Then Tricks On: the right stick does tricks instead of turning the camera.
2. Pull the right stick back (crouch), flick it up: an ollie. Back then up-left or up-right: a kickflip or heelflip (the board
   should spin once along its length). Back then left / right: a shove-it.
3. In the air: hold the left stick sideways to spin, hold R1 to grab. Land straight, or you bail.
4. Bail on purpose with Ragdoll Ratchet on: he should go limp. With Ragdoll off he falls off and gets back up.
5. On the ground, hold the right stick half back: a manual (nose up).

Copy back: `player/log.txt` (the lines start with `hoverboard:`). Tell me what felt wrong, and whether the kickflip and heelflip
go the right way round.

---

## PC test 4: ship canopy reflections (new tonight)

Start the game with **`Play-rt-canopy.cmd`**. Ray tracing mod on, with Reflections on, in the launcher.

1. Load a save: watch the ship's glass canopy as it lands. Press **Insert** to switch the reflections Off, Performance, Balanced
   and Ultra (the window title shows which).
2. Performance should look like before (the PS2's fixed shine). Balanced and Ultra should show the real sky and surroundings
   in the glass, clearer head on and shinier at an angle.
3. Look for: dark specks or dotted lines along the canopy's seams, flicker, or glass that looks too dark or too see-through on a
   bright planet (Novalis, Kerwan).

The full list, with GPU times to compare, is at the end of `RT_REFLECTIONS_PLAN.md` in
`.claude\worktrees\rt-refl-canopy-ed266c`. Screenshots from tonight (before / after) are in that folder's `shots`.

4. At Ultra, glass now uses two rays per pixel: the canopy and other glass should look smoother (less grainy or stair-stepped)
   than at Balanced, without costing much more.

Tell me how the canopy looks at each setting.

---

## After the tests

Both PC features are on their own branches (`claude/hoverboard-tricks-0de9a2`, `claude/rt-refl-canopy-ed266c`), not merged into
`mods-migration` yet. Tell me which ones to merge once you have seen them.
