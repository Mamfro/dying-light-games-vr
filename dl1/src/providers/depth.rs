//! Provider: D3D11 frame inputs (`monaka_stereo::providers::FrameInputs11`), for AFW
//! (`monaka_stereo::afw11::ready`) and the per-eye FSR upscaler
//! (`monaka_stereo::upscaler::fsr11`): which depth buffer is the scene's (the one bound last before
//! the scene reaches the back buffer, a D32S8 texture), and the frame's world image when it is
//! complete (the first draw after that copy, before the HUD: [`capture`], from the HUD's draws,
//! `crate::providers::hud_draws`). The cameras each frame was rendered with are recorded by the eye
//! site under the present number that chose its eye.

use crate::providers::eye_site;
use crate::{Dl1, Facts, swapchain_parts};
use monaka_hook::module::Module;
use monaka_producer::Rejection;
use monaka_stereo::afw11::ready as afw;
use monaka_stereo::providers::FrameInputs11;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::*};
use windows::Win32::Graphics::Direct3D11::{ID3D11DepthStencilView, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D};
use windows::core::Interface;

/// DL1's frame inputs: the scene depth kept at each scene copy, the device its swapchain's, the HUD
/// laid out in a band of its place (the UI kept at its fixed size, `crate::extras::ui_size`), the
/// published images shown to the layer probe.
impl FrameInputs11 for Dl1 {
    fn device(build: &Facts) -> Result<ID3D11Device, Rejection> {
        device(&build.renderer)
    }

    fn scene_depth(n: u64) -> Option<ID3D11Texture2D> {
        scene_depth(n)
    }

    fn hud_rect(rect: [f32; 4]) -> [f32; 4] {
        crate::extras::ui_size::fit_rect(rect)
    }

    fn probe(device: &ID3D11Device, context: &ID3D11DeviceContext, back: &ID3D11Texture2D, image: &ID3D11Texture2D, hud: Option<ID3D11Texture2D>, what: &str) {
        crate::research::layer::frame(device, context, back, image, hud, what);
    }
}

fn device(renderer: &Module) -> Result<ID3D11Device, Rejection> {
    swapchain_parts(renderer).map(|(_, device, _, _)| device)
}

static LAST_DEPTH: AtomicUsize = AtomicUsize::new(0);
/// The scene depth of the latest scene copy, and the present that shows it.
struct HeldDepth(Option<(u64, ID3D11Texture2D)>);
// SAFETY: the texture is only cloned under the mutex; D3D11 resources are free-threaded.
unsafe impl Send for HeldDepth {}
static SCENE_DEPTH: Mutex<HeldDepth> = Mutex::new(HeldDepth(None));

/// Whether AFW runs.
pub fn enabled() -> bool {
    afw::enabled()
}

/// Whether HUD draws go into AFW's HUD layer (laid over both eyes) instead of the back
/// buffer.
pub fn hud_layer() -> bool {
    afw::hud_layer()
}

/// The black and white HUD targets of this frame (each HUD draw goes into both).
pub fn hud_targets(context: &ID3D11DeviceContext, width: u32, height: u32) -> Option<[ID3D11RenderTargetView; 2]> {
    afw::hud_targets(context, width, height)
}

/// Whether this frame still has to be captured by AFW (checked before any work on the hot
/// draw path).
pub fn needs_capture() -> bool {
    afw::needs_capture()
}

/// The frame shown at present `n` (its world image is `color` now) to AFW, once per frame.
pub fn capture(context: &ID3D11DeviceContext, color: &ID3D11Texture2D, n: u64) {
    if let Some(schedule) = eye_site::schedule() {
        afw::capture(context, color, n, schedule);
    }
}

/// A depth view the game bound on the immediate context.
pub fn depth_bound(view: usize) {
    LAST_DEPTH.store(view, Relaxed);
}

/// The scene is being copied to the back buffer: the depth bound last in this frame is the
/// scene's. Its texture is kept with a reference of our own, under the present that shows the
/// frame, so no later frame reads a view the game may have released since.
pub fn scene_copied() {
    let view = LAST_DEPTH.swap(0, Relaxed) as *mut core::ffi::c_void;
    // SAFETY: a depth view the game bound earlier in this same frame (cleared at every present);
    // only its resource is taken, with its own reference.
    let texture = unsafe { ID3D11DepthStencilView::from_raw_borrowed(&view) }
        .and_then(|v| unsafe { v.GetResource() }.ok())
        .and_then(|r| r.cast::<ID3D11Texture2D>().ok());
    if let (Some(texture), Some(n)) = (texture, eye_site::drawing_present())
        && let Ok(mut held) = SCENE_DEPTH.lock()
    {
        held.0 = Some((n, texture));
    }
}

/// A present: depth bound before it belongs to the frame it showed.
pub fn frame_end() {
    LAST_DEPTH.store(0, Relaxed);
}

/// The scene depth of the frame shown at present `n` (tracked while AFW or FSR runs).
pub fn scene_depth(n: u64) -> Option<ID3D11Texture2D> {
    SCENE_DEPTH.lock().ok()?.0.as_ref().filter(|(at, _)| *at == n).map(|(_, texture)| texture.clone())
}

/// The scene depth let go (after the hooks are off).
pub fn release() {
    if let Ok(mut held) = SCENE_DEPTH.lock() {
        held.0 = None;
    }
}
