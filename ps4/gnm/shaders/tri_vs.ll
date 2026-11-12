; GNM triangle test, vertex shader (no vertex buffers: everything comes from the vertex number in v0).
; Vertices 0-2: a triangle that covers the whole screen (the background). Vertices 3-5: an RGB triangle turned by the angle
; the CPU writes into user data register 0 (s0) every frame.
define amdgpu_vs void @main(float inreg %t, i32 %vid) {
  %isbg = icmp ult i32 %vid, 3
  %k = urem i32 %vid, 3
  %kf = uitofp i32 %k to float
  %a0 = fmul float %kf, 2.0943951
  %a = fadd float %a0, %t
  %c = call float @llvm.cos.f32(float %a)
  %s = call float @llvm.sin.f32(float %a)
  ; 16:9 screen: x scaled by 9/16 so the triangle is not stretched.
  %tx0 = fmul float %c, 0.6
  %tx = fmul float %tx0, 0.5625
  %ty = fmul float %s, 0.6
  %k1 = icmp eq i32 %k, 1
  %k2 = icmp eq i32 %k, 2
  %bx = select i1 %k1, float 3.0, float -1.0
  %by = select i1 %k2, float 3.0, float -1.0
  %x = select i1 %isbg, float %bx, float %tx
  %y = select i1 %isbg, float %by, float %ty
  %k0 = icmp eq i32 %k, 0
  %tr = select i1 %k0, float 1.0, float 0.0
  %tg = select i1 %k1, float 1.0, float 0.0
  %tb = select i1 %k2, float 1.0, float 0.0
  %r = select i1 %isbg, float 0.05, float %tr
  %g = select i1 %isbg, float 0.05, float %tg
  %b = select i1 %isbg, float 0.25, float %tb
  call void @llvm.amdgcn.exp.f32(i32 32, i32 15, float %r, float %g, float %b, float 1.0, i1 false, i1 false)
  call void @llvm.amdgcn.exp.f32(i32 12, i32 15, float %x, float %y, float 0.0, float 1.0, i1 true, i1 false)
  ret void
}
declare float @llvm.cos.f32(float)
declare float @llvm.sin.f32(float)
declare void @llvm.amdgcn.exp.f32(i32, i32, float, float, float, float, i1, i1)
