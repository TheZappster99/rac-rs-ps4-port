//! Register byte addresses (AMD Sea Islands / CIK; Mesa src/amd/registers, sid.h). Context registers live at 0x28000+
//! (SET_CONTEXT_REG), persistent shader registers at 0xB000+ (SET_SH_REG), user-config registers at 0x30000+
//! (SET_UCONFIG_REG).

// Shader (SH) registers.
pub const SPI_SHADER_PGM_RSRC3_PS: u32 = 0xb01c;
pub const SPI_SHADER_USER_DATA_PS_0: u32 = 0xb030;
pub const SPI_SHADER_PGM_RSRC3_VS: u32 = 0xb118;
pub const SPI_SHADER_LATE_ALLOC_VS: u32 = 0xb11c;
pub const SPI_SHADER_USER_DATA_VS_0: u32 = 0xb130;

// Context registers.
pub const DB_RENDER_CONTROL: u32 = 0x28000;
pub const DB_DEPTH_VIEW: u32 = 0x28008;
pub const DB_HTILE_DATA_BASE: u32 = 0x28014;
pub const PA_SC_SCREEN_SCISSOR_TL: u32 = 0x28030;
pub const DB_DEPTH_INFO: u32 = 0x2803c;
pub const DB_Z_INFO: u32 = 0x28040;
pub const PA_SC_WINDOW_OFFSET: u32 = 0x28200;
pub const PA_SC_GENERIC_SCISSOR_TL: u32 = 0x28240;
pub const CB_TARGET_MASK: u32 = 0x28238;
pub const PA_SC_VPORT_SCISSOR_0_TL: u32 = 0x28250;
pub const PA_SC_VPORT_ZMIN_0: u32 = 0x282d0;
pub const PA_CL_VPORT_XSCALE: u32 = 0x2843c;
pub const SPI_PS_INPUT_CNTL_0: u32 = 0x28644;
pub const CB_BLEND0_CONTROL: u32 = 0x28780;
pub const DB_DEPTH_CONTROL: u32 = 0x28800;
pub const CB_COLOR_CONTROL: u32 = 0x28808;
pub const DB_SHADER_CONTROL: u32 = 0x2880c;
pub const PA_CL_CLIP_CNTL: u32 = 0x28810;
pub const PA_SU_SC_MODE_CNTL: u32 = 0x28814;
pub const PA_CL_VTE_CNTL: u32 = 0x28818;
pub const VGT_SHADER_STAGES_EN: u32 = 0x28b54;
pub const PA_SC_AA_CONFIG: u32 = 0x28be0;
pub const PA_SC_AA_MASK_X0Y0_X1Y0: u32 = 0x28c38;
pub const CB_COLOR0_BASE: u32 = 0x28c60;

// User-config registers.
pub const VGT_PRIMITIVE_TYPE: u32 = 0x30908;

/// PM4 type-3 opcodes.
pub mod op {
	pub const NOP: u32 = 0x10;
	pub const INDEX_TYPE: u32 = 0x2a;
	pub const NUM_INSTANCES: u32 = 0x2f;
	pub const EVENT_WRITE_EOP: u32 = 0x47;
	pub const ACQUIRE_MEM: u32 = 0x58;
	pub const SET_CONTEXT_REG: u32 = 0x69;
	pub const SET_SH_REG: u32 = 0x76;
	pub const SET_UCONFIG_REG: u32 = 0x79;
}

/// VGT_PRIMITIVE_TYPE values.
pub mod prim {
	pub const TRILIST: u32 = 4;
	pub const TRISTRIP: u32 = 6;
}

/// Depth / stencil compare functions (DB_DEPTH_CONTROL.ZFUNC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compare {
	Never = 0,
	Less = 1,
	Equal = 2,
	LessEqual = 3,
	Greater = 4,
	NotEqual = 5,
	GreaterEqual = 6,
	Always = 7,
}

/// DB_DEPTH_CONTROL: depth test on with `func`, writes on or off, no stencil.
pub fn db_depth_control(test: bool, write: bool, func: Compare) -> u32 {
	(test as u32) << 1 | (write as u32) << 2 | (func as u32) << 4
}

/// CB_BLEND0_CONTROL blend factors (BLEND_* values) and functions.
pub mod blend {
	pub const ZERO: u32 = 0;
	pub const ONE: u32 = 1;
	pub const SRC_COLOR: u32 = 2;
	pub const ONE_MINUS_SRC_COLOR: u32 = 3;
	pub const SRC_ALPHA: u32 = 4;
	pub const ONE_MINUS_SRC_ALPHA: u32 = 5;
	pub const DST_ALPHA: u32 = 6;
	pub const ONE_MINUS_DST_ALPHA: u32 = 7;
	pub const ADD: u32 = 0;
	pub const SUBTRACT: u32 = 1;
	pub const REVERSE_SUBTRACT: u32 = 4;

	/// CB_BLEND0_CONTROL: colour = src * sc op dst * dc, alpha likewise; `enable` false = no blending.
	pub fn control(enable: bool, sc: u32, op: u32, dc: u32, sa: u32, opa: u32, da: u32) -> u32 {
		if !enable {
			return 0;
		}
		sc | op << 5 | dc << 8 | sa << 16 | opa << 21 | da << 24 | 1 << 29 | 1 << 30
	}
}
