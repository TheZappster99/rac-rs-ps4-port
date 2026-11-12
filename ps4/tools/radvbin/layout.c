/* radvbin layout probe: prints the byte offsets of the fields ps4/tools/radvbin reads from radv's serialized shaders
 * (VK_KHR_pipeline_binary blobs), as Rust constants. The struct definitions below are copied verbatim from Mesa 26.1.6
 * (Debian mesa 26.1.6-1: src/amd/vulkan/radv_shader_info.h, radv_shader_args.h, radv_shader.h, src/amd/common/ac_binary.h,
 * ac_nir.h; MIT licence, Copyright Red Hat, Bas Nieuwenhuizen, Intel, Valve, AMD and the Mesa authors). The radv that
 * compiles the shaders (WSL Kali mesa-vulkan-drivers 26.1.6-1) must match this version; radvbin checks the blobs against
 * radv's own statistics and refuses on any mismatch.
 *   gcc -o layout layout.c && ./layout > layout.rs
 * Built with the same compiler and ABI (GCC, x86-64 SysV) as radv, so bitfields and padding come out the same. */
#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>

#define MAX_SETS 32
#define RADV_MAX_HEAPS 2
#define MAX_SO_BUFFERS 4
#define VARYING_SLOT_VAR0 32
#define MAX_VARYING 32
#define VARYING_SLOT_MAX (VARYING_SLOT_VAR0 + MAX_VARYING)
/* Enums only used as field types: their size and signedness are what matter (unsigned int, as in Mesa). */
typedef enum { MESA_SHADER_VERTEX = 0, MESA_SHADER_KERNEL = 14 } mesa_shader_stage;
enum tess_primitive_mode { TESS_PRIMITIVE_UNSPECIFIED = 0, TESS_PRIMITIVE_ISOLINES = 3 };
enum gl_tess_spacing { TESS_SPACING_UNSPECIFIED = 0, TESS_SPACING_FRACTIONAL_EVEN = 3 };
enum mesa_prim { MESA_PRIM_POINTS = 0, MESA_PRIM_UNKNOWN = 28 };

struct ac_shader_config {
   unsigned num_sgprs;
   unsigned num_vgprs;
   unsigned num_shared_vgprs;
   unsigned spilled_sgprs;
   unsigned spilled_vgprs;
   unsigned lds_size;
   unsigned spi_ps_input_ena;
   unsigned spi_ps_input_addr;
   unsigned float_mode;
   unsigned scratch_bytes_per_wave;
   bool wgp_mode;
   unsigned rsrc1;
   unsigned rsrc2;
   unsigned rsrc3;
};

typedef struct {
   uint64_t vram_output_mask;
   uint64_t lds_output_mask;
   uint64_t vgpr_output_mask;
   uint32_t vram_patch_output_mask;
   uint32_t lds_patch_output_mask;
   uint32_t vgpr_patch_output_mask;
   uint8_t highest_remapped_vram_output;
   uint8_t highest_remapped_vram_patch_output;
} ac_nir_tess_io_info;

enum radv_ud_index {
   AC_UD_SCRATCH_RING_OFFSETS = 0,
   AC_UD_PUSH_CONSTANTS = 1,
   AC_UD_INLINE_PUSH_CONSTANTS = 2,
   AC_UD_INDIRECT_DESCRIPTORS = 3,
   AC_UD_VIEW_INDEX = 4,
   AC_UD_STREAMOUT_BUFFERS = 5,
   AC_UD_STREAMOUT_STATE = 6,
   AC_UD_TASK_STATE = 7,
   AC_UD_NGGC_SETTINGS = 8,
   AC_UD_NGGC_VIEWPORT = 9,
   AC_UD_NGG_LDS_LAYOUT = 10,
   AC_UD_NGG_STATE = 11,
   AC_UD_NGG_QUERY_BUF_VA = 12,
   AC_UD_VGT_ESGS_RING_ITEMSIZE = 13,
   AC_UD_FORCE_VRS_RATES = 14,
   AC_UD_TASK_RING_ENTRY = 15,
   AC_UD_NEXT_STAGE_PC = 16,
   AC_UD_EPILOG_PC = 17,
   AC_UD_DYNAMIC_DESCRIPTORS = 18,
   AC_UD_DYNAMIC_DESCRIPTORS_OFFSET_ADDR = 19,
   AC_UD_SHADER_START = 20,
   AC_UD_VS_VERTEX_BUFFERS = AC_UD_SHADER_START,
   AC_UD_VS_BASE_VERTEX_START_INSTANCE,
   AC_UD_VS_PROLOG_INPUTS,
   AC_UD_VS_MAX_UD,
   AC_UD_PS_STATE,
   AC_UD_PS_MAX_UD,
   AC_UD_CS_GRID_SIZE = AC_UD_SHADER_START,
   AC_UD_CS_SBT_DESCRIPTORS,
   AC_UD_CS_RAY_LAUNCH_SIZE_ADDR,
   AC_UD_CS_RAY_DYNAMIC_CALLABLE_STACK_BASE,
   AC_UD_CS_TRAVERSAL_SHADER_ADDR,
   AC_UD_CS_TASK_RING_OFFSETS,
   AC_UD_CS_TASK_DRAW_ID,
   AC_UD_CS_TASK_IB,
   AC_UD_CS_MAX_UD,
   AC_UD_GS_MAX_UD,
   AC_UD_TCS_OFFCHIP_LAYOUT = AC_UD_VS_MAX_UD,
   AC_UD_TCS_MAX_UD,
   /* We might not know the previous stage when compiling a geometry shader, so we just
    * declare both TES and VS user SGPRs.
    */
   AC_UD_TES_MAX_UD = AC_UD_TCS_MAX_UD,
   AC_UD_MAX_UD = AC_UD_CS_MAX_UD,
};
struct radv_userdata_info {
   int8_t sgpr_idx;
   uint8_t num_sgprs;
};
struct radv_userdata_locations {
   struct radv_userdata_info descriptor_sets[MAX_SETS];
   struct radv_userdata_info descriptor_heaps[RADV_MAX_HEAPS];
   struct radv_userdata_info shader_data[AC_UD_MAX_UD];
   uint32_t descriptor_sets_enabled;
   uint32_t descriptor_heaps_enabled;
};
enum radv_shader_type {
   RADV_SHADER_TYPE_DEFAULT = 0,
   RADV_SHADER_TYPE_GS_COPY,
   RADV_SHADER_TYPE_TRAP_HANDLER,
   RADV_SHADER_TYPE_RT_PROLOG,
   RADV_SHADER_TYPE_RT_TRAVERSAL,
};


struct radv_vs_output_info {
   uint8_t vs_output_param_offset[VARYING_SLOT_MAX];
   uint8_t clip_dist_mask;
   uint8_t cull_dist_mask;
   uint8_t param_exports;
   uint8_t prim_param_exports;
   bool writes_pointsize;
   bool writes_layer;
   bool writes_layer_per_primitive;
   bool writes_viewport_index;
   bool writes_viewport_index_per_primitive;
   bool writes_primitive_shading_rate;
   bool writes_primitive_shading_rate_per_primitive;
   bool export_prim_id;
   bool export_prim_id_per_primitive;
};

struct radv_streamout_info {
   uint16_t strides[MAX_SO_BUFFERS];
   uint32_t enabled_stream_buffers_mask;
};

struct radv_legacy_gs_info {
   uint32_t gs_inst_prims_in_subgroup;
   uint32_t es_verts_per_subgroup;
   uint32_t gs_prims_per_subgroup;
   uint32_t esgs_itemsize;
   uint32_t lds_size;
};

struct gfx10_ngg_info {
   uint16_t ngg_emit_size; /* in dwords */
   uint32_t hw_max_esverts;
   uint32_t max_gsprims;
   uint32_t max_out_verts;
   uint32_t prim_amp_factor;
   uint32_t vgt_esgs_ring_itemsize;
   uint32_t esgs_ring_size;
   uint32_t lds_size;
   bool max_vert_out_per_gs_instance;
};

struct radv_shader_info {
   uint32_t workgroup_size;
   uint32_t nir_shared_size;
   uint64_t inline_push_constant_mask;
   uint32_t push_constant_size;
   uint32_t desc_set_used_mask;
   uint32_t user_data_0;
   uint32_t num_tess_patches;
   uint32_t esgs_itemsize;       /* Only for VS or TES as ES */
   uint32_t ngg_lds_vertex_size; /* VS,TES: Cull+XFB, GS: GSVS size */
   uint64_t gs_inputs_read;  /* Mask of GS inputs read (only used by linked ES) */

   struct radv_userdata_locations user_sgprs_locs;
   struct radv_vs_output_info outinfo;

   uint8_t wave_size;
   uint8_t ngg_lds_scratch_size;
   mesa_shader_stage stage : 8;
   mesa_shader_stage next_stage : 8;
   enum radv_shader_type type : 4;

   bool can_inline_all_push_constants : 1;
   bool loads_push_constants : 1;
   bool loads_dynamic_offsets : 1;
   bool loads_dynamic_descriptors_offset_addr : 1;
   bool uses_view_index : 1;
   bool uses_invocation_id : 1;
   bool uses_prim_id : 1;
   bool is_ngg : 1;
   bool is_ngg_passthrough : 1;
   bool has_ngg_culling : 1;
   bool has_ngg_early_prim_export : 1;
   bool has_prim_query : 1;
   bool has_xfb_query : 1;
   bool force_vrs_per_vertex : 1;
   bool inputs_linked : 1;
   bool outputs_linked : 1;
   bool merged_shader_compiled_separately : 1; /* GFX9+ */
   bool force_indirect_descriptors : 1;
   bool ngg_wave_id_en : 1;
   bool descriptor_heap : 1;

   struct {
      uint64_t tcs_inputs_via_temp;
      uint64_t tcs_inputs_via_lds;
      uint32_t vb_desc_usage_mask;
      uint32_t input_slot_usage_mask;
      uint32_t num_outputs; /* For NGG streamout only */
      uint8_t num_linked_outputs;
      uint8_t num_attributes;
      bool needs_draw_id : 1;
      bool needs_instance_id : 1;
      bool as_es : 1;
      bool as_ls : 1;
      bool tcs_in_out_eq : 1;
      bool needs_base_instance : 1;
      bool use_per_attribute_vb_descs : 1;
      bool has_prolog : 1;
      bool dynamic_inputs : 1;
      bool dynamic_num_verts_per_prim : 1;
   } vs;
   struct {
      uint8_t num_components_per_stream[4];
      uint32_t vertices_in;
      uint32_t vertices_out;
      uint32_t input_prim;
      uint32_t output_prim;
      uint32_t invocations;
      uint32_t es_type; /* GFX9: VS or TES */
      uint8_t num_linked_inputs;
      bool has_pipeline_stat_query;
   } gs;
   struct {
      uint32_t tcs_vertices_out;
      uint32_t num_outputs;            /* For NGG streamout only */
      uint8_t num_linked_inputs;       /* Number of reserved per-vertex input slots in VRAM. */
      uint8_t num_linked_patch_inputs; /* Number of reserved per-patch input slots in VRAM. */
      uint8_t num_linked_outputs;
      enum tess_primitive_mode _primitive_mode : 2;
      enum gl_tess_spacing spacing : 2;
      bool as_es : 1;
      bool ccw : 1;
      bool point_mode : 1;
      bool reads_tess_factors : 1;
   } tes;
   struct {
      uint32_t input_mask;
      uint32_t input_per_primitive_mask;
      uint32_t float32_shaded_mask;
      uint32_t explicit_shaded_mask;
      uint32_t explicit_strict_shaded_mask;
      uint32_t float16_shaded_mask;
      uint32_t float16_hi_shaded_mask;
      uint32_t num_inputs;
      uint8_t input_clips_culls_mask;
      bool uses_sample_shading : 1;
      bool needs_sample_positions : 1;
      bool needs_poly_line_smooth : 1;
      bool writes_memory : 1;
      bool writes_z : 1;
      bool writes_stencil : 1;
      bool writes_sample_mask : 1;
      bool writes_mrt0_alpha : 1;
      bool mrt0_is_dual_src : 1;
      bool exports_mrtz_via_epilog : 1;
      bool has_pcoord : 1;
      bool prim_id_input : 1;
      bool viewport_index_input : 1;
      bool can_discard : 1;
      bool early_fragment_test : 1;
      bool post_depth_coverage : 1;
      bool uses_fbfetch_output : 1;
      uint8_t reads_frag_coord_mask;
      uint8_t reads_sample_pos_mask;
      uint8_t depth_layout;
      bool reads_sample_mask_in : 1;
      bool reads_front_face : 1;
      bool reads_sample_id : 1;
      bool reads_frag_shading_rate : 1;
      bool reads_barycentric_model : 1;
      bool reads_persp_sample : 1;
      bool reads_persp_center : 1;
      bool reads_persp_centroid : 1;
      bool reads_linear_sample : 1;
      bool reads_linear_center : 1;
      bool reads_linear_centroid : 1;
      bool reads_fully_covered : 1;
      bool reads_pixel_coord : 1;
      bool reads_layer : 1;
      bool pops : 1; /* Uses Primitive Ordered Pixel Shading (fragment shader interlock) */
      bool pops_is_per_sample : 1;
      uint32_t spi_ps_input_ena;
      uint32_t spi_ps_input_addr;
      uint32_t colors_written; /* Mask of outputs written */
      uint32_t spi_shader_col_format;
      uint32_t cb_shader_mask;
      uint8_t color0_written;
      bool load_provoking_vtx : 1;
      bool load_rasterization_prim : 1;
      bool force_sample_iter_shading_rate : 1;
      bool allow_flat_shading : 1;

      bool has_epilog : 1;
   } ps;
   struct {
      uint32_t block_size[3];
      bool uses_block_id[3];
      bool uses_thread_id[3];
      bool uses_grid_size : 1;
      bool uses_local_invocation_idx : 1;

      bool uses_full_subgroups : 1;
      bool linear_taskmesh_dispatch : 1;
      bool has_query : 1; /* Task shader only */

      uint32_t derivative_group : 2;
   } cs;
   struct {
      ac_nir_tess_io_info io_info;
      uint64_t tes_inputs_read;
      uint64_t tes_patch_inputs_read;
      uint32_t tcs_vertices_out;
      uint32_t lds_size;         /* in bytes */
      uint8_t num_linked_inputs; /* Number of reserved per-vertex input slots in LDS. */
      enum gl_tess_spacing spacing;
      bool ccw;
      bool point_mode;
      bool tes_reads_tess_factors : 1;
   } tcs;
   struct {
      enum mesa_prim output_prim : 8; /* byte-size aligned */
      bool needs_ms_scratch_ring;
      bool has_task; /* If mesh shader is used together with a task shader. */
      bool has_query;
   } ms;

   struct radv_streamout_info so;

   union {
      struct radv_legacy_gs_info legacy_gs_info;
      struct gfx10_ngg_info ngg_info;
   };
};

struct radv_shader_binary {
   uint32_t type; /* enum radv_shader_binary_type */

   struct ac_shader_config config;
   struct radv_shader_info info;

   /* Self-referential size so we avoid consistency issues. */
   uint32_t total_size;
};
struct radv_shader_binary_legacy {
   struct radv_shader_binary base;
   uint32_t code_size;
   uint32_t exec_size;
   uint32_t ir_size;
   uint32_t disasm_size;
   uint32_t stats_size;
   uint32_t debug_info_size;

   /* data has size of stats_size + code_size + ir_size + disasm_size + 2,
    * where the +2 is for 0 of the ir strings. */
   uint8_t data[0];
};

#define B(f) (offsetof(struct radv_shader_binary, f))
#define P(name, val) printf("pub const %s: usize = %zu;\n", name, (size_t)(val))

int main(void) {
   printf("// Generated by ps4/tools/radvbin/layout.c for Mesa 26.1.6 (do not edit).\n");
   P("TYPE", B(type));
   P("CONFIG", B(config));
   P("CONFIG_NUM_SGPRS", B(config.num_sgprs));
   P("CONFIG_NUM_VGPRS", B(config.num_vgprs));
   P("CONFIG_SPILLED_SGPRS", B(config.spilled_sgprs));
   P("CONFIG_SPILLED_VGPRS", B(config.spilled_vgprs));
   P("CONFIG_LDS_SIZE", B(config.lds_size));
   P("CONFIG_SPI_PS_INPUT_ENA", B(config.spi_ps_input_ena));
   P("CONFIG_SPI_PS_INPUT_ADDR", B(config.spi_ps_input_addr));
   P("CONFIG_FLOAT_MODE", B(config.float_mode));
   P("CONFIG_SCRATCH_BYTES_PER_WAVE", B(config.scratch_bytes_per_wave));
   P("CONFIG_RSRC1", B(config.rsrc1));
   P("CONFIG_RSRC2", B(config.rsrc2));
   P("CONFIG_RSRC3", B(config.rsrc3));
   P("INFO", B(info));
   P("INFO_WORKGROUP_SIZE", B(info.workgroup_size));
   P("INFO_INLINE_PUSH_CONSTANT_MASK", B(info.inline_push_constant_mask));
   P("INFO_PUSH_CONSTANT_SIZE", B(info.push_constant_size));
   P("INFO_DESC_SET_USED_MASK", B(info.desc_set_used_mask));
   P("INFO_USER_DATA_0", B(info.user_data_0));
   P("INFO_UD_DESCRIPTOR_SETS", B(info.user_sgprs_locs.descriptor_sets));
   P("INFO_UD_SHADER_DATA", B(info.user_sgprs_locs.shader_data));
   P("INFO_UD_SETS_ENABLED", B(info.user_sgprs_locs.descriptor_sets_enabled));
   P("AC_UD_MAX", AC_UD_MAX_UD);
   P("INFO_VS_OUTPUT_PARAM_OFFSET", B(info.outinfo.vs_output_param_offset));
   P("VARYING_SLOT_MAX", VARYING_SLOT_MAX);
   P("INFO_PARAM_EXPORTS", B(info.outinfo.param_exports));
   P("INFO_CLIP_DIST_MASK", B(info.outinfo.clip_dist_mask));
   P("INFO_CULL_DIST_MASK", B(info.outinfo.cull_dist_mask));
   P("INFO_WRITES_POINTSIZE", B(info.outinfo.writes_pointsize));
   P("INFO_WAVE_SIZE", B(info.wave_size));
   P("INFO_VS_VB_DESC_USAGE_MASK", B(info.vs.vb_desc_usage_mask));
   P("INFO_VS_INPUT_SLOT_USAGE_MASK", B(info.vs.input_slot_usage_mask));
   P("INFO_VS_NUM_ATTRIBUTES", B(info.vs.num_attributes));
   P("INFO_PS_INPUT_MASK", B(info.ps.input_mask));
   P("INFO_PS_FLOAT32_SHADED_MASK", B(info.ps.float32_shaded_mask));
   P("INFO_PS_EXPLICIT_SHADED_MASK", B(info.ps.explicit_shaded_mask));
   P("INFO_PS_FLOAT16_SHADED_MASK", B(info.ps.float16_shaded_mask));
   P("INFO_PS_NUM_INPUTS", B(info.ps.num_inputs));
   P("INFO_PS_SPI_PS_INPUT_ENA", B(info.ps.spi_ps_input_ena));
   P("INFO_PS_SPI_PS_INPUT_ADDR", B(info.ps.spi_ps_input_addr));
   P("INFO_PS_COLORS_WRITTEN", B(info.ps.colors_written));
   P("INFO_PS_SPI_SHADER_COL_FORMAT", B(info.ps.spi_shader_col_format));
   P("INFO_PS_CB_SHADER_MASK", B(info.ps.cb_shader_mask));
   P("INFO_CS_BLOCK_SIZE", B(info.cs.block_size));
   P("TOTAL_SIZE", B(total_size));
   P("BINARY_SIZE", sizeof(struct radv_shader_binary));
   P("LEGACY_CODE_SIZE", offsetof(struct radv_shader_binary_legacy, code_size));
   P("LEGACY_EXEC_SIZE", offsetof(struct radv_shader_binary_legacy, exec_size));
   P("LEGACY_IR_SIZE", offsetof(struct radv_shader_binary_legacy, ir_size));
   P("LEGACY_DISASM_SIZE", offsetof(struct radv_shader_binary_legacy, disasm_size));
   P("LEGACY_STATS_SIZE", offsetof(struct radv_shader_binary_legacy, stats_size));
   P("LEGACY_DEBUG_INFO_SIZE", offsetof(struct radv_shader_binary_legacy, debug_info_size));
   P("LEGACY_DATA", offsetof(struct radv_shader_binary_legacy, data));
   return 0;
}
