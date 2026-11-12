//! PS4 port (ps4/app, target os freebsd): stand-ins for the few winit pieces the viewer drives (the event loop, the window, the
//! application handler trait). The console has no window system, so winit cannot open anything there; with these, the viewer's own
//! `ApplicationHandler` code (handler.rs) runs unchanged: `resumed` once, then one `RedrawRequested` per frame, as fast as the
//! frame's own pacing lets it. The "window" is the GNM back end's 1920x1080 surface, reached through a dummy raw window handle (the
//! back end ignores it). winit's event types are still used as they are (they are plain data and compile everywhere).
//! PC builds never compile this file: viewer.rs picks winit's real types there.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use winit::event::{DeviceEvent, DeviceId, WindowEvent};
use winit::raw_window_handle as rwh;

/// The surface size: the back end's video-out buffers are always 1920x1080 (the console scales them to the TV mode).
pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;

/// Frames drawn so far (the PS4 main loop's count, for the app's log).
pub static FRAMES: AtomicU64 = AtomicU64::new(0);

/// The surface as the viewer sees its window.
pub struct Window;

impl Window {
	/// winit's own attributes type: the viewer's builder calls (title, size, icon) run as on the PC and are then ignored.
	pub fn default_attributes() -> winit::window::WindowAttributes {
		Default::default()
	}

	pub fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
		winit::dpi::PhysicalSize::new(WIDTH, HEIGHT)
	}

	pub fn has_focus(&self) -> bool {
		true
	}

	pub fn set_title(&self, _title: &str) {}

	pub fn request_redraw(&self) {}

	pub fn set_cursor_visible(&self, _visible: bool) {}

	pub fn set_cursor_grab(&self, _mode: winit::window::CursorGrabMode) -> Result<(), std::io::Error> {
		Ok(())
	}

	pub fn set_fullscreen(&self, _f: Option<winit::window::Fullscreen>) {}

	/// The console's surface stays 1920x1080: a size request (the display tests' RAC_TEST_FULLSCREEN) changes nothing.
	pub fn request_inner_size(&self, _size: winit::dpi::PhysicalSize<u32>) -> Option<winit::dpi::PhysicalSize<u32>> {
		None
	}

	pub fn current_monitor(&self) -> Option<Monitor> {
		Some(Monitor)
	}
}

/// The TV, for the frame cap's refresh rate.
pub struct Monitor;

impl Monitor {
	pub fn refresh_rate_millihertz(&self) -> Option<u32> {
		Some(60000)
	}
}

// The GNM back end takes any handle (there is one surface: video out), as ps4/wgputest does.
impl rwh::HasWindowHandle for Window {
	fn window_handle(&self) -> Result<rwh::WindowHandle<'_>, rwh::HandleError> {
		// SAFETY: a dummy handle that nothing dereferences.
		Ok(unsafe { rwh::WindowHandle::borrow_raw(rwh::RawWindowHandle::Web(rwh::WebWindowHandle::new(1))) })
	}
}

impl rwh::HasDisplayHandle for Window {
	fn display_handle(&self) -> Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
		// SAFETY: as above.
		Ok(unsafe { rwh::DisplayHandle::borrow_raw(rwh::RawDisplayHandle::Web(rwh::WebDisplayHandle::new())) })
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowId;

/// What the handler gets as `el`: it can only ask to quit and open the (one) window.
#[derive(Default)]
pub struct ActiveEventLoop {
	exit: AtomicBool,
}

impl ActiveEventLoop {
	pub fn exit(&self) {
		self.exit.store(true, Ordering::Relaxed);
	}

	pub fn create_window(&self, _attrs: winit::window::WindowAttributes) -> Result<Window, std::io::Error> {
		Ok(Window)
	}
}

/// winit's trait with the three methods the viewer implements, over the stand-in event loop.
pub trait ApplicationHandler {
	fn resumed(&mut self, el: &ActiveEventLoop);
	fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent);
	fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, _event: DeviceEvent) {}
}

pub struct EventLoop;

impl EventLoop {
	pub fn new() -> Result<EventLoop, std::io::Error> {
		Ok(EventLoop)
	}

	/// The PS4 main loop: the window comes up at once, then frames until the game quits. RAC_PS4_FRAMES=N stops after N frames
	/// (test runs; ps4/app sets it from frames.txt).
	pub fn run_app<A: ApplicationHandler>(self, app: &mut A) -> Result<(), std::io::Error> {
		let el = ActiveEventLoop::default();
		let limit = std::env::var("RAC_PS4_FRAMES").ok().and_then(|v| v.trim().parse::<u64>().ok()).filter(|&n| n > 0);
		app.resumed(&el);
		while !el.exit.load(Ordering::Relaxed) {
			app.window_event(&el, WindowId, WindowEvent::RedrawRequested);
			let n = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
			if limit.is_some_and(|l| n >= l) {
				eprintln!("ps4: {n} frames drawn, stopping (RAC_PS4_FRAMES)");
				break;
			}
		}
		Ok(())
	}
}
