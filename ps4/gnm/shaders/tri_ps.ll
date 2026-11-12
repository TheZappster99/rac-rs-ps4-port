; GNM triangle test, pixel shader: the interpolated colour (VS parameter 0) to colour target 0.
; s0 = the primitive mask (no user data), v0/v1 = the perspective-centre barycentrics.
define amdgpu_ps void @main(i32 inreg %prim, <2 x float> %psample, <2 x float> %pcenter) {
  %i = extractelement <2 x float> %pcenter, i32 0
  %j = extractelement <2 x float> %pcenter, i32 1
  %r1 = call float @llvm.amdgcn.interp.p1(float %i, i32 0, i32 0, i32 %prim)
  %r = call float @llvm.amdgcn.interp.p2(float %r1, float %j, i32 0, i32 0, i32 %prim)
  %g1 = call float @llvm.amdgcn.interp.p1(float %i, i32 1, i32 0, i32 %prim)
  %g = call float @llvm.amdgcn.interp.p2(float %g1, float %j, i32 1, i32 0, i32 %prim)
  %b1 = call float @llvm.amdgcn.interp.p1(float %i, i32 2, i32 0, i32 %prim)
  %b = call float @llvm.amdgcn.interp.p2(float %b1, float %j, i32 2, i32 0, i32 %prim)
  call void @llvm.amdgcn.exp.f32(i32 0, i32 15, float %r, float %g, float %b, float 1.0, i1 true, i1 true)
  ret void
}
declare float @llvm.amdgcn.interp.p1(float, i32, i32, i32)
declare float @llvm.amdgcn.interp.p2(float, float, i32, i32, i32)
declare void @llvm.amdgcn.exp.f32(i32, i32, float, float, float, float, i1, i1)
