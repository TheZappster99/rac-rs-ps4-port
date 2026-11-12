; GNM back end (rac-rs/vendor/wgpu-hal/src/gnm/blit.rs): the vertex shader of the back end's own clear / copy draws.
; No vertex buffers: vertices 0-2 are one triangle covering the whole target. User data 0 (s0) = the depth it writes (clip
; z, w = 1). One parameter (zeros) is exported so the vertex shader always has a parameter export.
define amdgpu_vs void @main(float inreg %z, i32 %vid) {
  %k1 = icmp eq i32 %vid, 1
  %k2 = icmp eq i32 %vid, 2
  %x = select i1 %k1, float 3.0, float -1.0
  %y = select i1 %k2, float 3.0, float -1.0
  call void @llvm.amdgcn.exp.f32(i32 32, i32 15, float 0.0, float 0.0, float 0.0, float 0.0, i1 false, i1 false)
  call void @llvm.amdgcn.exp.f32(i32 12, i32 15, float %x, float %y, float %z, float 1.0, i1 true, i1 false)
  ret void
}
declare void @llvm.amdgcn.exp.f32(i32, i32, float, float, float, float, i1, i1)
