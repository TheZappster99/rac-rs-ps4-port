//! [PC] Enhanced image (post.rs), moved out of `draw_passes` (a pure move, lane ndlss): the level pass has ended; the water
//! views and water pass, smoke, fog, underwater, bloom / grade / GI write the graded picture back into the MSAA target, then
//! the HUD pass begins. Split view: `draw_passes` calls `post_frame` only under `DrawFrame.post_on` = `splitview::fx`
//! (prepare.rs), so the Classic half never runs it; its upscaler inputs are paused there too (split.rs, `Prep::pause`).

use super::*;

impl Gpu {
	/// The post passes of a Lighting Enhanced frame (no pass may be open) and the HUD pass begun after them. `water` = the
	/// level pass's water draws, the water frame and the sun rays; `on` = (part_on, covered, smoke_on); `target` = (view,
	/// gi_level, rs) of the frame. None only without a scene (draw_passes has one).
	#[allow(clippy::too_many_arguments)]
	pub(super) fn post_frame<'e>(&mut self, mut enc: &'e mut wgpu::CommandEncoder, cuts: &mut super::presenter::Cuts, cam: &Camera, dc: &DrawFrame, db: &DrawBufs, water: (&mut Vec<super::waterfx::WaterDraw>, &Option<super::waterfx::WaterFrame>, Option<([f32; 2], f32)>), on: (bool, bool, bool), fx_lights: &super::partfx::FxLights, target: (&wgpu::TextureView, u8, bool), rt_rays: Option<[f32; 3]>) -> Option<wgpu::RenderPass<'e>> {
		let scene = self.scene.as_ref()?;
		let (aspect, wfar, gfx, u, multi) = (dc.aspect, dc.wfar, dc.gfx, &dc.u, &db.multi);
		let ((wrec, water_frame, rays), (part_on, covered, smoke_on), (view, gi_level, rs)) = (water, on, target);
		cuts.cut(&self.device, &mut enc, "f_fx");
		// [PC] The planar mirrors (mirror.rs) of the flat water planes below the eye the view looks at most: Water High up to
		// three planes (each its own pass and layer), Medium the one, Low none (the reflection march alone, no second scene pass).
		// [PC] Ray-traced reflections at Ultra (rtrefl.rs `mirrors_traced`): the traced water covers the flat planes too, so no
		// plane is mirrored and the mirror's pass over the level is saved, once the traced water can run (structure with hit data).
		let traced = super::rtrefl::mirrors_traced() && self.waterfx.traces() && scene.tex_array.is_some() && self.sun.rt.as_ref().is_some_and(|rt| rt.hit().is_some());
		let picked: Vec<f32> = water_frame.as_ref().map(|f| mirror_pick(self.level, &wrec, cam.pos, &f.vp, if traced { 0 } else { mirror_count(gfx.water_q) })).unwrap_or_default();
		let mirrors = self.waterfx.steady_mirrors(picked); self.firefx.set_water(&wrec);
		self.log_water(&wrec, &mirrors, water_frame.as_ref(), cam.pos);
		let water = match (&water_frame, self.post.water_inputs()) {
			// (Also with no water in view but a waterfall, which the water pass draws.)
			(Some(f), Some(inputs)) if !wrec.is_empty() || self.waterfx.has_falls() => {
				self.waterfx.positions(&self.device, &self.queue, &mut enc, &self.depth, inputs, f, &wrec, &mirrors, part_on || covered);
				cuts.cut(&self.device, &mut enc, "f_wpos");
				self.pass_water_views(enc, cuts, scene, u, &mirrors, multi.as_ref().map(|m| &m.0));
				true
			}
			_ => false,
		};
		// [PC] Graphics -> Underwater look Off (under_choice.rs, read once per frame here): the underwater look and the sky map see a dry
		// camera; the volumetric fog gate (prepare.rs) keeps the raw flag, so under water retail's fog keeps its full strength.
		let under_look = crate::viewer::under_choice::look(self.under_flag);
		self.sky_build(&mut enc, scene, u, cam.pos, water_frame.is_some() && (water || under_look || self.under.active()));
		self.post.bind_water(&self.device, if water { self.waterfx.gi_mask() } else { None });
		let waterfx = &self.waterfx;
		let fall_tex =super::waterfx::fall_fx_tex(self.level).first().and_then(|t| self.fx_tex.get(t));
		// [PC] Water Low / Medium / High: the underwater look (underwater.rs), after the water pass, before bloom and the grade.
		let under_in = water_frame.as_ref().map(|f| super::underwater::frame(cam, aspect, under_look, self.under_surf, &wrec, f.fog, f.key.map(|k| k.0), f.level, wfar));
		// [PC] Ray-traced shadows (rtshadow.rs): this frame's shadow mask from the level pass's depth, read by the next frame.
		if let (Some(f), Some(rt), true) = (&dc.sun, &mut self.sun.rt, super::rtshadow::shadows()) {
			rt.mask(&self.device, &self.queue, &mut enc, &self.depth, cam, aspect, (RETAIL_NEAR, wfar), f.dir, (self.config.width, self.config.height));
		}
		// [PC] Volumetric fog (volfog.rs): after the water pass, before bloom and the grade; not under water (its own look).
		let fog = super::volfog::Input::new(&self.post, &self.sun, dc.sun.as_ref(), dc.fog, u, (self.level, self.config.width, self.config.height), (cam, aspect, wfar), &self.sun_rows());
		let fog = fog.map(|mut f| { f.water = water.then(|| self.waterfx.gi_mask().map(|m| m.0)).flatten(); f });
		let smoke_in = if smoke_on { super::volsmoke::Input::new(&self.post, &self.sun, dc.sun.as_ref(), u, (self.level, self.config.width, self.config.height), (cam, aspect, wfar), &self.sun_rows()) } else { None };
		let fire_cores = if smoke_on { self.firefx.cores() } else { Vec::new() }; // [PC] volsmoke/fire_cut.rs
		// [PC] volsmoke.rs runs before the fog, which covers it.
		// [PC] Ray-traced reflections (rtrefl.rs): the water traces through the level's structure with the scene's textures.
		let rt_water = self.sun.rt.as_ref().zip(scene.tex_array.as_ref()).map(|(rt, array)| (&self.device, rt, array));
		let (volfog, volsmoke, under, device, queue, depth) = (&mut self.volfog, &mut self.volsmoke, &mut self.under, &self.device, &self.queue, &self.depth);
		self.post.encode(&self.queue, &mut enc, cam, aspect, gfx.ao_q, gfx.sun_q, gfx.particles_q, part_on, &fx_lights, rays, rt_rays.is_some(), gi_level, wfar, |enc, scene, ao| {
			if water {
				waterfx.shade(enc, scene, ao, fall_tex, rt_water);
			}
			if let Some(si) = &smoke_in {
				volsmoke.run(device, queue, enc, si, &fx_lights, &fire_cores);
			}
			if let Some(fog) = &fog { volfog.run(device, queue, enc, fog, depth); }
			if let Some(uf) = &under_in {
				under.run(device, queue, enc, scene, depth, uf, waterfx.sky());
			}
		});
		// [PC] Metal On: next frame's reflection source (metalenv.rs).
		self.metal_capture_ssr(enc, u);
		cuts.cut(&self.device, &mut enc, "f_pe");
		// [PC] Ray-traced sun rays (rtrays.rs).
		if let (Some(col), Some(f), Some(rt), Some(rr)) = (rt_rays, &dc.sun, &self.sun.rt, &mut self.sun.rt_rays) {
			rr.run(&self.device, &self.queue, &mut enc, rt, &self.post, cam, aspect, f, col, gfx.sun_q);
		}
		// [PC] RAC_POST_VIEW=rtrefl (rtrefl.rs): what the camera's rays hit, traced and shaded, over the rays target.
		if let (Some(rt), Some(rd), Some(array)) = (&self.sun.rt, &mut self.sun.rt_refl, scene.tex_array.as_ref()) {
			rd.run(&self.device, &self.queue, &mut enc, rt, &self.post, cam, aspect, array);
		}
		self.post.composite(&mut enc, &self.msaa, gi_level);
		cuts.cut(&self.device, &mut enc, "f_post");
		self.upscale_encode(enc, cuts); // [PC] RAC_UPSCALE_PREP (upscale.rs): the upscaler inputs (+ object motion), where ReShade takes its own
		Some(self.hud_pass(enc, cuts, view, rs))
	}
}
