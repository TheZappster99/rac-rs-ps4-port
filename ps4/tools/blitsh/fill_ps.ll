; GNM back end blit.rs: clear colour. User data 0-3 (s0-s3) = the colour, exported as 32-bit floats to colour target 0.
define amdgpu_ps void @main(float inreg %r, float inreg %g, float inreg %b, float inreg %a, <2 x float> %pcenter) {
  call void @llvm.amdgcn.exp.f32(i32 0, i32 15, float %r, float %g, float %b, float %a, i1 true, i1 true)
  ret void
}
declare void @llvm.amdgcn.exp.f32(i32, i32, float, float, float, float, i1, i1)
