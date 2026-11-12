//! The GNM layer of the PS4 port (PS4_GPU_DESIGN.md, milestone 1): what a renderer needs to draw on the PS4's GPU (AMD GCN
//! GFX7, "Liverpool") without Sony's SDK.
//!
//! - `pm4`: PM4 type-3 command buffers (register writes, draws, end-of-pipe labels) written into GPU-visible memory.
//! - `regs`: the register addresses used (AMD Sea Islands / CIK, as in Mesa's sid.h; checked against shadPS4's map).
//! - `desc`: GFX7 resource descriptors: buffers (V#), 2D textures (T#), samplers (S#).
//! - `target`: colour and depth target registers.
//! - `shader`: shader-cache entries (.rsh, written by ps4/tools/radvbin from Mesa radv's GFX7 binaries) and the register
//!   values for the driver's sceGnmSetVsShader / sceGnmSetPsShader350 and the PS input mapping.
//! - `cache`: the shader cache file (every captured pipeline, compiled on the PC; ps4/tools/shcache writes it).
//! - `ps4` (console only): direct memory, the driver calls, video out.
//!
//! Proven on the console so far (ps4/gnm): PM4 register writes, sceGnmDrawInitDefaultHardwareState350 / SetVsShader /
//! SetPsShader350 / DrawIndexAuto, EVENT_WRITE_EOP labels, linear colour targets (tile mode index 8) with COMP_SWAP ALT.
//! Everything else here is untested on hardware until ps4/gnmtest runs.

pub mod cache;
pub mod desc;
pub mod pm4;
pub mod regs;
pub mod shader;
pub mod target;

#[cfg(target_os = "freebsd")]
pub mod ps4;
