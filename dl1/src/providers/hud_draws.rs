//! Provider: the HUD layer on D3D11 (the HUD kept out of the frame, as `monaka_stereo::hud11`'s
//! handlers).
//!
//! HUD and menus, on Monaka's D3D11 HUD placement (`monaka_stereo::hud11`): the frame's 3D image
//! reaches the back buffer as one unblended three-vertex draw, and every later draw to the back
//! buffer in that frame is HUD or menu. Those draws get a viewport at the HUD's place in the eye, where
//! the frame labels put it (`crate::providers::frame_labels`).
//!
//! This module is Dying Light's own facts and routing, as `hud11`'s handlers: only the immediate
//! context drawing, each frame waiting for its own scene copy; the first draw after the scene copy
//! taken by AFW and the upscaler (the world, before the HUD); with either, the HUD drawn
//! into their layer instead of placed; the hands' panels (Monaka's dynamic HUD, by [`crate::providers::hud_pieces`]) taking their
//! pieces' draws; the fixed-size UI ([`crate::extras::ui_size`]) laid out in a band of the
//! viewport; and the probes.

use crate::providers::{depth, eye_site, fsr};
use crate::{Dl1, swapchain_parts};
use monaka_stereo::hud11::{self, Handlers, Route, SceneCopy, Setup};
use monaka_hook::Hooks;
use monaka_producer::Rejection;
use monaka_producer::game::{Lack, Start};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use windows::Win32::Graphics::Direct3D11::{D3D11_VIEWPORT, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;
use windows::core::Interface;

/// The draws component's install (`crate::DRAWS`): the HUD set up for the back buffer when it is
/// placed or the upscaler takes it, the immediate context's draws followed.
pub(crate) fn install_draws(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = crate::config(start);
    let (_, _, context, (width, height)) = swapchain_parts(&start.build.renderer)?;
    if config.hud_active() || config.upscaled().is_some() {
        install((width, height));
    }
    enable();
    // The HUD is placed for itself and for the upscaler's layer; otherwise its draws are only
    // followed.
    let distance = (config.hud_active() || config.upscaled().is_some()).then_some(config.hud.distance);
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
        backbuffer_draw: Some(crate::research::backbuffer::draw),
        scene_copied: Some(scene_copied),
        hud_draw: Some(hud_draw),
        layout: Some(crate::extras::ui_size::layout_band),
        placed: Some(placed),
        run: Some(run),
    };
    let setup = Setup { move_scissor: false, give_back_on_leave: false, handlers, ..Setup::new(scale, distance, SceneCopy::EachFrame) };
    hud11::install_with::<Dl1>(hooks, setup, Some(context))?;
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
    crate::research::targets::frame_end();
    crate::providers::hud_pieces::hud_phase(false);
    // SAFETY: COM calls on the game's live swapchain.
    let context = crate::providers::hud_pieces::active().then(|| unsafe { chain.GetDevice::<windows::Win32::Graphics::Direct3D11::ID3D11Device>().and_then(|d| d.GetImmediateContext()) }.ok()).flatten();
    crate::providers::hud_pieces::frame_end(context.as_ref());
}

fn immediate(context: &ID3D11DeviceContext) -> bool {
    // SAFETY: COM call on a live context.
    unsafe { context.GetType() == windows::Win32::Graphics::Direct3D11::D3D11_DEVICE_CONTEXT_IMMEDIATE }
}

/// `OMSetRenderTargets` on the immediate context: the probes, and the depth AFW and the
/// upscaler read and the G-buffer's object motion.
fn targets(context: &ID3D11DeviceContext, on: bool, count: u32, views: *const *mut c_void, depth: *mut c_void) {
    crate::research::backbuffer::bound(context, on, count, !depth.is_null());
    crate::research::targets::bound(count, views, !depth.is_null());
    crate::research::depth::bound(depth);
    if !depth.is_null() && (depth::enabled() || fsr::enabled()) {
        depth::depth_bound(depth as usize);
    }
    if fsr::enabled() {
        fsr::bound(count, views, !depth.is_null());
    }
}

/// Draw calls the hooks saw: whether they stay hooked while the game runs.
static DRAWS_SEEN: AtomicU64 = AtomicU64::new(0);

pub fn draws_seen() -> u64 {
    DRAWS_SEEN.load(Relaxed)
}

/// Every draw: its arguments kept for the HUD probe, counted, and the render-target probe's.
fn any_draw(context: *mut c_void, draw: &hud11::Draw) {
    note_draw(DrawArgs { indexed: draw.indexed, count: draw.count, start: draw.start, base: draw.base });
    DRAWS_SEEN.fetch_add(1, Relaxed);
    if crate::research::targets::enabled()
        && eye_site::capturing()
        // SAFETY: the game passes its live context.
        && unsafe { ID3D11DeviceContext::from_raw_borrowed(&context) }.is_some_and(immediate)
    {
        crate::research::targets::draw(draw.count);
    }
}

/// The frame's 3D image reached the back buffer: the probes, AFW's scene depth, and the
/// panels' HUD phase (the HUD's matrices caught from here to the present).
fn scene_copied(context: &ID3D11DeviceContext) {
    SCENE_COPIES.fetch_add(1, Relaxed);
    crate::research::depth::scene_copied();
    crate::research::targets::scene_copied(context);
    depth::scene_copied();
    crate::providers::hud_pieces::hud_phase(true);
}

/// A HUD draw. The first after the scene copy captures the world for AFW and the upscaler.
/// With either, the HUD goes at the game's own layout into their layer, each eye placing it when
/// composited (by AFW, or over the upscaled eye); otherwise it is placed.
fn hud_draw(context: &ID3D11DeviceContext, vertices: u32) -> Route {
    // The first draw after the scene copy: the back buffer holds the world, not yet the HUD.
    if (depth::needs_capture() || fsr::needs_capture())
        && let Some(frame) = eye_site::drawing_present()
    {
        let mut target = [None];
        // SAFETY: reads the bound render target into a local.
        unsafe { context.OMGetRenderTargets(Some(&mut target), None) };
        // SAFETY: COM calls on the bound render target view and its resource.
        let image = target[0].as_ref().and_then(|view| unsafe { view.GetResource() }.ok()).and_then(|r| r.cast::<ID3D11Texture2D>().ok());
        if let Some(image) = image {
            depth::capture(context, &image, frame);
            fsr::capture(context, &image, frame);
        }
    }
    if crate::providers::hud_pieces::on_hud_draw(context, vertices) {
        SKIP_DRAW.store(true, Relaxed);
    }
    if eye_site::drawing_present().is_none() {
        return Route::Leave;
    }
    if !(depth::hud_layer() || fsr::enabled()) {
        return Route::Place;
    }
    // The size is copied out: the layer owners take their own locks, and the present holds theirs
    // while asking for the HUD's rectangles.
    let Some((width, height)) = backbuffer_size() else { return Route::Leave };
    // The fixed UI (`extras::ui_size`) goes into an FSR layer of its own shape and size.
    let fixed_layer = crate::extras::ui_size::fixed().filter(|_| !depth::hud_layer()).map(crate::extras::ui_size::layer_size);
    match fixed_layer {
        Some(size) => crate::extras::ui_size::fill_draw(context, size),
        None => crate::extras::ui_size::band_draw(context),
    }
    let (width, height) = fixed_layer.unwrap_or((width, height));
    // The hands' panels take their pieces' draws here too (the rest go into the layer): the
    // layout is the game's own viewport, unmoved in this path.
    if crate::providers::hud_pieces::active() {
        let mut count = 1u32;
        let mut viewport = D3D11_VIEWPORT::default();
        // SAFETY: reads one viewport into a local.
        unsafe { context.RSGetViewports(&mut count, Some(&mut viewport)) };
        if count > 0 {
            crate::providers::hud_pieces::note_viewports(viewport);
            PANEL_DRAW.store(true, Relaxed);
        }
    }
    let targets = if depth::hud_layer() { depth::hud_targets(context, width, height) } else { fsr::hud_targets(context, width, height) };
    targets.map_or(Route::Leave, Route::Layer)
}

/// A HUD draw placed: counted, and the hands' panels map from its layout.
fn placed(layout: D3D11_VIEWPORT) {
    SHIFTED_DRAWS.fetch_add(1, Relaxed);
    if crate::providers::hud_pieces::active() {
        crate::providers::hud_pieces::note_viewports(layout);
        PANEL_DRAW.store(true, Relaxed);
    }
}

/// Runs a draw once, or, for a HUD draw that belongs in the HUD layer, once into each of its two
/// targets (black and white) and then gives the game its own target binding back. A HUD draw of a
/// piece on the hands goes into its panel instead, either way ([`crate::providers::hud_pieces::draw_hud`]).
/// The binding goes past the hook ([`hud11::draw_into_layer`]), so the HUD's tracking does not see
/// it.
fn run(context: *mut c_void, layer: Option<&[ID3D11RenderTargetView; 2]>, draw: &dyn Fn()) {
    draw_routed(context, layer, draw);
    crate::extras::ui_size::after_draw(context);
}

fn draw_routed(context: *mut c_void, layer: Option<&[ID3D11RenderTargetView; 2]>, draw: &dyn Fn()) {
    if SKIP_DRAW.swap(false, Relaxed) {
        return;
    }
    let as_made = || match layer {
        Some(targets) => {
            // SAFETY: the game's immediate context, in its hooked draw.
            if unsafe { hud11::draw_into_layer(context, targets, draw) } {
                LAYER_DRAWS.fetch_add(1, Relaxed);
            } else {
                draw();
            }
        }
        None => draw(),
    };
    if PANEL_DRAW.swap(false, Relaxed) {
        // A piece's draw goes into its panel as it is; any other the way it would have gone.
        return crate::providers::hud_pieces::draw_hud(context, draw, &as_made);
    }
    as_made()
}

static LAYER_DRAWS: AtomicU64 = AtomicU64::new(0);

/// A draw call's own arguments (the vertex or index count, the first index or vertex, the base
/// vertex), for the HUD probe's geometry (`research::hud`, with the panels): draws run on the
/// immediate context's one thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawArgs {
    pub indexed: bool,
    pub count: u32,
    pub start: u32,
    pub base: i32,
}

static DRAW_ARGS: Mutex<DrawArgs> = Mutex::new(DrawArgs { indexed: false, count: 0, start: 0, base: 0 });

fn note_draw(args: DrawArgs) {
    if crate::providers::hud_pieces::active()
        && let Ok(mut last) = DRAW_ARGS.lock()
    {
        *last = args;
    }
}

/// The arguments of the draw being run.
pub fn draw_args() -> DrawArgs {
    DRAW_ARGS.lock().map(|a| *a).unwrap_or_default()
}
/// The HUD draw just seen stays undrawn ([`crate::providers::hud_pieces`]); draws run on the immediate context's
/// one thread, so a flag carries it from the check to the draw.
static SKIP_DRAW: AtomicBool = AtomicBool::new(false);
/// The HUD draw just placed also goes into the HUD panels ([`crate::providers::hud_pieces::draw_hud`]).
static PANEL_DRAW: AtomicBool = AtomicBool::new(false);

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

    /// The HUD placed by DL1's frame labels through `hud11` is where DL1's own placement put it:
    /// in the frustum the eye is rendered with, the eyes the configured separation apart, at the
    /// HUD's distance and size; over both eyes, several poses and image sizes (its draws and the
    /// rectangles its layer is laid over each eye at).
    #[test]
    fn the_hud_goes_where_dl1s_own_placement_put_it() {
        let config = Config::from_options(&Options::parse("hud_distance=1.5\nhud_scale=0.4"), &crate::manifest::MANIFESTS[0]);
        eye_site::install(eye_site::Setup { config, camera: CameraWriter(no_camera) });
        let (distance, scale) = (config.hud.distance, config.hud.scale);
        for pose in poses(&config) {
            for eye in [0, 1] {
                let own = config.rendered_frustum(&pose, eye).map(|frustum| Placement::new(frustum, eye, config.separation_for(&pose), distance));
                let placed = hud11::place_eye::<Dl1>(&pose, eye, distance);
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
