//! Provider: the HUD layer on D3D11 (the HUD kept out of the frame, as `monaka_stereo::hud11`'s
//! handlers).
//!
//! HUD and menus, on Monaka's D3D11 HUD placement (`monaka_stereo::hud11`): the frame's 3D image
//! reaches the back buffer as one unblended three-vertex draw, and every later draw to the back
//! buffer in that frame is HUD or menu. Those draws get a viewport at the HUD's place in the eye,
//! where the frame labels put it (`crate::providers::frame_labels`).
//!
//! This module is the engine's own facts and routing (Dying Light 1's, which Riptide's renderer
//! shares), as `hud11`'s handlers: only the immediate context drawing, each frame waiting for its
//! own scene copy; the first draw after the scene copy taken by AFW (the world, before the HUD);
//! with AFW, the HUD drawn into its layer instead of placed.

use crate::providers::{depth, eye_site};
use crate::{Riptide, swapchain_parts};
use monaka_hook::Hooks;
use monaka_producer::Rejection;
use monaka_producer::game::{Lack, Start};
use monaka_stereo::hud11::{self, Handlers, Route, SceneCopy, Setup};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use windows::Win32::Graphics::Direct3D11::{ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;
use windows::core::Interface;

/// The draws component's install (`crate::DRAWS`): the HUD set up for the back buffer when it is
/// placed, the immediate context's draws followed.
pub(crate) fn install_draws(start: &mut Start<'_, Riptide>) -> Result<(), Lack> {
    let config = crate::config(start);
    let (_, _, context, (width, height)) = swapchain_parts(&start.build.renderer)?;
    if config.hud_active() {
        install((width, height));
    }
    enable();
    // The HUD is placed when shown; otherwise its draws are only followed.
    let distance = config.hud_active().then_some(config.hud.distance);
    hook(start.hooks, &context, config.hud.scale, distance)?;
    Ok(())
}

/// The back buffer's size as the HUD was set up with (width, height).
static BACKBUFFER: Mutex<Option<(u32, u32)>> = Mutex::new(None);

/// The HUD is set up for a back buffer of `size` (width, height).
pub fn install(size: (u32, u32)) {
    *BACKBUFFER.lock().unwrap_or_else(|e| e.into_inner()) = Some(size);
}

static SHIFTED_DRAWS: AtomicU64 = AtomicU64::new(0);
/// Frames whose 3D image was seen reaching the back buffer (the HUD routing starts there).
static SCENE_COPIES: AtomicU64 = AtomicU64::new(0);
static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn enable() {
    ENABLED.store(true, Release);
}

pub fn shifted_draws() -> u64 {
    SHIFTED_DRAWS.load(Relaxed)
}

/// The back buffer's size as the HUD was set up with.
pub fn backbuffer_size() -> Option<(u32, u32)> {
    *BACKBUFFER.lock().ok()?
}

pub fn scene_copies() -> u64 {
    SCENE_COPIES.load(Relaxed)
}

/// Hooks the game's immediate context `context` for its HUD (`scale`: the HUD's size as a share
/// of the image; `distance`: how far away it is placed, `None` not placed): its draws followed
/// while the eyes are captured.
pub fn hook(hooks: &mut Hooks, context: &ID3D11DeviceContext, scale: f32, distance: Option<f32>) -> Result<(), Rejection> {
    let handlers = Handlers {
        following: Some(eye_site::capturing),
        draw: Some(any_draw),
        targets: Some(targets),
        scene_copied: Some(scene_copied),
        hud_draw: Some(hud_draw),
        placed: Some(placed),
        run: Some(run),
        ..Handlers::default()
    };
    let setup = Setup { move_scissor: false, give_back_on_leave: false, handlers, ..Setup::new(scale, distance, SceneCopy::EachFrame) };
    hud11::install_with::<Riptide>(hooks, setup, Some(context))?;
    hud11::ACTIVE.store(true, Release);
    Ok(())
}

/// Called at each present: the next frame starts; remember this swapchain's back buffer and
/// leave the game's own viewport behind.
pub fn frame_end(chain: &IDXGISwapChain) {
    if !ENABLED.load(Acquire) {
        return;
    }
    hud11::frame_end(chain);
    depth::frame_end();
}

/// `OMSetRenderTargets` on the immediate context: the depth AFW reads.
fn targets(_: &ID3D11DeviceContext, _: bool, _: u32, _: *const *mut c_void, depth: *mut c_void) {
    if !depth.is_null() && depth::enabled() {
        depth::depth_bound(depth as usize);
    }
}

/// Draw calls the hooks saw: whether they stay hooked while the game runs.
static DRAWS_SEEN: AtomicU64 = AtomicU64::new(0);

pub fn draws_seen() -> u64 {
    DRAWS_SEEN.load(Relaxed)
}

/// Every draw, counted.
fn any_draw(_: *mut c_void, _: &hud11::Draw) {
    DRAWS_SEEN.fetch_add(1, Relaxed);
}

/// The frame's 3D image reached the back buffer: AFW's scene depth.
fn scene_copied(_: &ID3D11DeviceContext) {
    SCENE_COPIES.fetch_add(1, Relaxed);
    depth::scene_copied();
}

/// A HUD draw. The first after the scene copy captures the world for AFW. With AFW, the HUD goes
/// at the game's own layout into its layer, each eye placing it when composited; otherwise it is
/// placed.
fn hud_draw(context: &ID3D11DeviceContext, _: u32) -> Route {
    // The first draw after the scene copy: the back buffer holds the world, not yet the HUD.
    if depth::needs_capture()
        && let Some(frame) = eye_site::drawing_present()
    {
        let mut target = [None];
        // SAFETY: reads the bound render target into a local.
        unsafe { context.OMGetRenderTargets(Some(&mut target), None) };
        // SAFETY: COM calls on the bound render target view and its resource.
        let image = target[0].as_ref().and_then(|view| unsafe { view.GetResource() }.ok()).and_then(|r| r.cast::<ID3D11Texture2D>().ok());
        if let Some(image) = image {
            depth::capture(context, &image, frame);
        }
    }
    if eye_site::drawing_present().is_none() {
        return Route::Leave;
    }
    if !depth::hud_layer() {
        return Route::Place;
    }
    // The size is copied out: the layer owner takes its own locks, and the present holds its own
    // while asking for the HUD's rectangles.
    let Some((width, height)) = backbuffer_size() else { return Route::Leave };
    depth::hud_targets(context, width, height).map_or(Route::Leave, Route::Layer)
}

/// A HUD draw placed: counted.
fn placed(_: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT) {
    SHIFTED_DRAWS.fetch_add(1, Relaxed);
}

/// Runs a draw once, or, for a HUD draw that belongs in the HUD layer, once into each of its two
/// targets (black and white) and then gives the game its own target binding back. The binding goes
/// past the hook ([`hud11::draw_into_layer`]), so the HUD's tracking does not see it.
fn run(context: *mut c_void, layer: Option<&[ID3D11RenderTargetView; 2]>, draw: &dyn Fn()) {
    match layer {
        Some(targets) => {
            // SAFETY: the game's immediate context, in its hooked draw.
            if unsafe { hud11::draw_into_layer(context, targets, draw) } {
                LAYER_DRAWS.fetch_add(1, Relaxed);
            } else {
                draw();
            }
        }
        None => draw(),
    }
}

static LAYER_DRAWS: AtomicU64 = AtomicU64::new(0);

pub fn layer_draws() -> u64 {
    LAYER_DRAWS.load(Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::providers::engine::CameraWriter;
    use monaka_core::hud::{Placement, Rect};
    use monaka_core::options::Options;
    use monaka_core::protocol::HeadPose;

    unsafe extern "system" fn no_camera(_: *mut c_void, _: *const f32) {}

    /// Head poses: none, straight ahead, turned and pitched, and one with a headset's own fields
    /// of view and eye distance.
    fn poses(config: &Config) -> Vec<HeadPose> {
        let mut headset = config.synthetic_pose(-20.0, 5.0);
        headset.fov = [[-0.9, 0.75, 0.82, -0.88], [-0.75, 0.9, 0.82, -0.88]];
        headset.ipd = 0.07;
        vec![HeadPose::default(), config.synthetic_pose(0.0, 0.0), config.synthetic_pose(35.0, -10.0), headset]
    }

    /// The HUD placed by Riptide's frame labels through `hud11` is in the frustum the eye is
    /// rendered with, the eyes the configured separation apart, at the HUD's distance and size;
    /// over both eyes, several poses and image sizes.
    #[test]
    fn the_hud_goes_in_the_rendered_frustum() {
        let config = Config::from_options(&Options::parse("hud_distance=1.5\nhud_scale=0.4"), &crate::manifest::MANIFESTS[0]);
        eye_site::install(eye_site::Setup { config, camera: CameraWriter(no_camera) });
        let (distance, scale) = (config.hud.distance, config.hud.scale);
        for pose in poses(&config) {
            for eye in [0, 1] {
                let own = config.rendered_frustum(&pose, eye).map(|frustum| Placement::new(frustum, eye, config.separation_for(&pose), distance));
                let placed = hud11::place_eye::<Riptide>(&pose, eye, distance);
                assert_eq!(placed, own, "eye {eye}, pose {pose:?}");
                assert!(placed.is_some() || pose.valid == 0, "a tracked pose has a place");
                for (width, height) in [(1920.0, 1080.0), (2560.0, 1440.0), (1440.0, 1600.0), (3840.0, 2160.0)] {
                    let base = Rect::sized(width, height);
                    let rect = |at: Option<Placement>| at.map_or(base.array(), |at| at.rect(base, base.aspect(), scale).array());
                    assert_eq!(rect(placed), rect(own), "eye {eye}, {width}x{height}");
                }
            }
        }
    }
}
