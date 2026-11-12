	.section	.AMDGPU.config,"",@progbits
	.long	45096
	.long	0
	.long	165608
	.long	0
	.long	45100
	.long	0
	.long	165580
	.long	1
	.long	165584
	.long	1
	.long	4
	.long	0
	.long	8
	.long	0
	.text
	.globl	main                            ; -- Begin function main
	.p2align	8
	.type	main,@function
main:                                   ; @main
; %bb.0:
	v_mov_b32_e32 v0, s0
	v_mov_b32_e32 v1, s1
	v_mov_b32_e32 v2, s2
	v_mov_b32_e32 v3, s3
	exp mrt0, v0, v1, v2, v3 done vm
	s_endpgm
.Lfunc_end0:
	.size	main, .Lfunc_end0-main
                                        ; -- End function
	.set .Lmain.num_vgpr, 4
	.set .Lmain.num_agpr, 0
	.set .Lmain.numbered_sgpr, 4
	.set .Lmain.num_named_barrier, 0
	.set .Lmain.private_seg_size, 0
	.set .Lmain.uses_vcc, 0
	.set .Lmain.uses_flat_scratch, 0
	.set .Lmain.has_dyn_sized_stack, 0
	.set .Lmain.has_recursion, 0
	.set .Lmain.has_indirect_call, 0
	.section	.AMDGPU.csdata,"",@progbits
; Kernel info:
; codeLenInByte = 28
; TotalNumSgprs: 4
; NumVgprs: 4
; ScratchSize: 0
; MemoryBound: 0
; FloatMode: 240
; IeeeMode: 0
; LDSByteSize: 0 bytes/workgroup (compile time only)
; SGPRBlocks: 0
; VGPRBlocks: 0
; NumSGPRsForWavesPerEU: 4
; NumVGPRsForWavesPerEU: 4
; Occupancy: 10
; WaveLimiterHint : 0
; COMPUTE_PGM_RSRC2:SCRATCH_EN: 0
; COMPUTE_PGM_RSRC2:USER_SGPR: 0
; COMPUTE_PGM_RSRC2:TRAP_HANDLER: 0
; COMPUTE_PGM_RSRC2:TGID_X_EN: 0
; COMPUTE_PGM_RSRC2:TGID_Y_EN: 0
; COMPUTE_PGM_RSRC2:TGID_Z_EN: 0
; COMPUTE_PGM_RSRC2:TIDIG_COMP_CNT: 0
	.section	.AMDGPU.gpr_maximums,"",@progbits
	.set amdgpu.max_num_vgpr, 0
	.set amdgpu.max_num_agpr, 0
	.set amdgpu.max_num_sgpr, 0
	.set amdgpu.max_num_named_barrier, 0
	.section	.AMDGPU.csdata,"",@progbits
	.section	".note.GNU-stack","",@progbits
	.amd_amdgpu_isa "amdgcn-unknown-unknown-unknown-gfx701"
