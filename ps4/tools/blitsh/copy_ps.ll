; GNM back end blit.rs: copy (render-pass resolve). User data 0-7 (s0-s7) = the source's image descriptor; each pixel loads
; the source texel at its own position (no filtering) and exports it as 32-bit floats to colour target 0.
; The arguments after the descriptor are the pixel shader inputs in hardware order (SPI_PS_INPUT_ADDR bits 0-9); only
; POS_X / POS_Y (bits 8, 9) are read.
define amdgpu_ps void @main(<8 x i32> inreg %t, <2 x float> %ps, <2 x float> %pc, <2 x float> %pcd, <3 x float> %pull,
    <2 x float> %ls, <2 x float> %lc, <2 x float> %lcd, float %stip, float %px, float %py) {
  %x = fptoui float %px to i32
  %y = fptoui float %py to i32
  %c = call <4 x float> @llvm.amdgcn.image.load.2d.v4f32.i32(i32 15, i32 %x, i32 %y, <8 x i32> %t, i32 0, i32 0)
  %r = extractelement <4 x float> %c, i32 0
  %g = extractelement <4 x float> %c, i32 1
  %b = extractelement <4 x float> %c, i32 2
  %a = extractelement <4 x float> %c, i32 3
  call void @llvm.amdgcn.exp.f32(i32 0, i32 15, float %r, float %g, float %b, float %a, i1 true, i1 true)
  ret void
}
declare <4 x float> @llvm.amdgcn.image.load.2d.v4f32.i32(i32, i32, i32, <8 x i32>, i32, i32)
declare void @llvm.amdgcn.exp.f32(i32, i32, float, float, float, float, i1, i1)
