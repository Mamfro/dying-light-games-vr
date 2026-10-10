//! FSR upscaling (`fsr=1`, [`monaka_fsr11`]): the game renders each eye smaller, its projection
//! shifted by that eye's next FSR jitter ([`jitter_for`], applied in the view setup). At the first
//! draw after the scene copy (the world, before the HUD) the frame is upscaled with motion vectors
//! from its depth and the eye's cameras ([`capture`]); the HUD goes into a layer that is laid over
//! the upscaled eye at present ([`publish`]). Frames with no 3D view (a movie, a loading screen)
//! have nothing to upscale: they go to the viewer's flat screen, resampled to the eye size.
//!
//! The G-buffer's fourth target (R16G16_FLOAT) holds the game's object motion, the camera left out
//! (`research::targets`); its units are not known, so it is only added with `fsr_object_motion`.

use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16G16_FLOAT;
use crate::config;
use crate::view::stereo;
use monaka_channel::pose::HeadSource;
use monaka_core::alternate::{PresentRing, Schedule};
use monaka_core::camera::Frustum;
use monaka_core::protocol::HeadPose;
use monaka_fsr11::{EyeFrame, Fsr11, Settings};
use monaka_framegen::upscale::create;
use monaka_producer::log;
use monaka_stereo::alternate::{AlternatePublisher, Step};
use monaka_stereo::hybrid::EyeCameras;
use monaka_warp::{EyeView, HudLayer, ObjectMotion, Scaler};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::time::Instant;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D};
use windows::core::Interface;

/// A frame's cameras and the jitter its projection was shifted by, kept under the present after
/// which its eye was chosen.
#[derive(Clone, Copy)]
struct Recorded {
    cameras: Option<EyeCameras>,
    jitter: [f32; 2],
}

struct State {
    fsr: Option<Fsr11>,
    settings: Option<config::Fsr>,
    hud: Option<HudLayer>,
    hud_begun: bool,
    hud_views: Option<[ID3D11RenderTargetView; 2]>,
    recorded: PresentRing<Recorded>,
    gbuffer: Option<ID3D11Texture2D>,
    /// This frame's upscaled eye: the present that shows it, and the image.
    output: Option<(u64, ID3D11Texture2D)>,
    /// Flat frames resampled to the eye size.
    scaler: Option<Scaler>,
    /// Frames in a row drawn with no 3D view.
    viewless: u32,
    flat: u64,
    last: [Option<(Instant, [f32; 3])>; 2],
    upscaled: u64,
    busy: u64,
    missing: u64,
    failures: u64,
}

struct Cell(State);
// SAFETY: the D3D objects inside are used under the mutex on the game's render thread, or by the
// stop after the hooks are off.
unsafe impl Send for Cell {}

static STATE: Mutex<Cell> = Mutex::new(Cell(State {
    fsr: None,
    settings: None,
    hud: None,
    hud_begun: false,
    hud_views: None,
    recorded: PresentRing::new(),
    gbuffer: None,
    output: None,
    scaler: None,
    viewless: 0,
    flat: 0,
    last: [None, None],
    upscaled: 0,
    busy: 0,
    missing: 0,
    failures: 0,
}));
static ENABLED: AtomicBool = AtomicBool::new(false);
static CAPTURED: AtomicBool = AtomicBool::new(false);

/// Loads AMD's DLLs from `dlls` and makes the upscaler for `render` to `display`; returns a line
/// for the log.
pub fn install(device: &ID3D11Device, settings: config::Fsr, render: (u32, u32), display: (u32, u32), dlls: &Path) -> Result<String, String> {
    let api = monaka_fsr11::load(dlls).map_err(|e| format!("FSR: {e}"))?;
    // DL1's depth buffer is reversed with an infinite far plane (`engine::depth_buffer_projection`).
    let fsr = Fsr11::new(
        device,
        api,
        Settings {
            render,
            display,
            sharpness: settings.sharpness,
            jitter_sign: settings.jitter_sign,
            depth_flags: create::DEPTH_INVERTED | create::DEPTH_INFINITE,
        },
    )
    .map_err(|e| format!("FSR: {e}"))?;
    let hud = HudLayer::new(device).map_err(|e| format!("FSR HUD layer: {e}"))?;
    let scaler = Scaler::new(device).map_err(|e| format!("FSR flat frames: {e}"))?;
    let line = format!("FSR {}: {}x{} upscaled to {}x{}, {} jitter phases, {settings:?}", fsr.version(), render.0, render.1, display.0, display.1, fsr.phases());
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.0.fsr = Some(fsr);
    state.0.settings = Some(settings);
    state.0.hud = Some(hud);
    state.0.scaler = Some(scaler);
    drop(state);
    ENABLED.store(true, Release);
    Ok(line)
}

pub fn enabled() -> bool {
    ENABLED.load(Acquire)
}

/// The jitter (pixels, x right, y down) for the frame whose eye `eye` was chosen after present
/// `chosen_at`: the eye's next one the first time, the same one for every later view setup of that
/// frame. `None` without FSR.
pub fn jitter_for(chosen_at: u64, eye: usize) -> Option<[f32; 2]> {
    if !enabled() {
        return None;
    }
    let mut state = STATE.lock().ok()?;
    let state = &mut state.0;
    if let Some(known) = state.recorded.get(chosen_at) {
        return Some(known.jitter);
    }
    let jitter = state.fsr.as_mut()?.jitter(eye);
    state.recorded.put(chosen_at, Recorded { cameras: None, jitter });
    Some(jitter)
}

/// `frustum` shifted so the image content moves by `jitter` pixels (x right, y down) at the
/// render size.
pub fn jittered(frustum: Frustum, jitter: [f32; 2]) -> Frustum {
    let Some((width, height)) = STATE.lock().ok().and_then(|s| s.0.fsr.as_ref().map(Fsr11::render)) else { return frustum };
    // Content moves +dx in normalised device x when the edges move -dx * (right - left) / 2.
    let dx = 2.0 * jitter[0] / width as f32 * (frustum.right - frustum.left) / 2.0;
    let dy = -2.0 * jitter[1] / height as f32 * (frustum.up - frustum.down) / 2.0;
    Frustum { left: frustum.left - dx, right: frustum.right - dx, up: frustum.up - dy, down: frustum.down - dy }
}

/// The view setup's cameras (unjittered) of the frame chosen after present `chosen_at`, and the
/// jitter actually written into its projection.
pub fn record(chosen_at: u64, cameras: EyeCameras, jitter: [f32; 2]) {
    if let Ok(mut state) = STATE.lock() {
        state.0.recorded.put(chosen_at, Recorded { cameras: Some(cameras), jitter });
    }
}

/// The game bound `count` render targets: keeps the G-buffer's fourth (object motion).
pub fn bound(count: u32, views: *const *mut core::ffi::c_void, depth: bool) {
    let Some(texture) = motion_target(count, views, depth) else { return };
    let Ok(mut state) = STATE.lock() else { return };
    if state.0.gbuffer.as_ref().is_none_or(|known| known.as_raw() != texture.as_raw()) {
        state.0.gbuffer = Some(texture);
    }
}

/// Whether this frame still has to be upscaled (checked before any work on the hot draw path).
pub fn needs_capture() -> bool {
    enabled() && !CAPTURED.load(Acquire)
}

/// The vertical field of view of a canonical projection.
fn fov_vertical(projection: &[f32; 16]) -> f32 {
    let (scale, offset) = (projection[5], projection[6]);
    ((offset + 1.0) / scale).atan() - ((offset - 1.0) / scale).atan()
}

/// Upscales the frame shown at present `n` (its world image is `color` now), once per frame.
pub fn capture(context: &ID3D11DeviceContext, color: &ID3D11Texture2D, n: u64) {
    if !enabled() || CAPTURED.swap(true, AcqRel) {
        return;
    }
    let Some(schedule) = stereo::schedule() else { return };
    let eye = schedule.eye_at(n);
    let chosen_at = schedule.chosen_at(n);
    let depth = crate::output::hybrid::scene_depth(n);
    let Ok(mut state) = STATE.lock() else { return };
    let state = &mut state.0;
    // No view setup wrote this frame's camera (each records its jitter): no 3D view.
    state.viewless = if state.recorded.get(chosen_at).is_none() { state.viewless.saturating_add(1) } else { 0 };
    let recorded = state.recorded.get(chosen_at).filter(|_| Schedule::camera_eye(chosen_at) == eye);
    let (Some(depth), Some(Recorded { cameras: Some(cameras), jitter, .. })) = (depth, recorded) else {
        state.missing += 1;
        return;
    };
    let projection = cameras.projection[eye];
    let camera = cameras.camera[eye];
    let position = [camera[3], camera[7], camera[11]];
    let now = Instant::now();
    // A jump of more than 2 m between an eye's frames is a cut: its history goes.
    let (frame_time_ms, reset) = match state.last[eye] {
        Some((then, at)) => {
            let moved = (0..3).map(|i| (position[i] - at[i]).powi(2)).sum::<f32>().sqrt();
            ((now - then).as_secs_f32() * 1000.0, moved > 2.0)
        }
        None => (22.0, true),
    };
    state.last[eye] = Some((now, position));
    let object_motion = state.settings.map_or(0.0, |s| s.object_motion);
    let object = state.gbuffer.as_ref().filter(|_| object_motion != 0.0).map(|texture| ObjectMotion { texture, scale: [object_motion; 2] });
    let frame = EyeFrame {
        color,
        depth: &depth,
        object,
        view: EyeView { camera, projection },
        jitter,
        near: projection[11],
        fov_vertical: fov_vertical(&projection),
        frame_time_ms,
        reset,
    };
    let Some(fsr) = state.fsr.as_mut() else { return };
    match fsr.upscale(context, eye, &frame) {
        Ok(image) => {
            state.output = Some((n, image));
            state.upscaled += 1;
        }
        Err(monaka_fsr11::Error::Busy) => state.busy += 1,
        Err(e) => {
            monaka_producer::log_first!(1, "FSR upscale failed: {e}");
            state.failures += 1;
        }
    }
}

/// The black and white targets this frame's HUD draws go into, begun at its first HUD draw.
pub fn hud_targets(context: &ID3D11DeviceContext, width: u32, height: u32) -> Option<[ID3D11RenderTargetView; 2]> {
    let mut state = STATE.lock().ok()?;
    let state = &mut state.0;
    if state.hud_begun {
        return state.hud_views.clone();
    }
    match state.hud.as_mut()?.begin(context, width, height) {
        Ok(views) => {
            state.hud_begun = true;
            state.hud_views = Some(views.clone());
            Some(views)
        }
        Err(e) => {
            monaka_producer::log_first!(1, "FSR HUD layer unavailable: {e}");
            state.failures += 1;
            None
        }
    }
}

/// Frames in a row with no 3D view before they go to the flat screen: a frame now and then without
/// one (a level streaming in) does not flash it.
const FLAT_AFTER: u32 = 4;

/// At present `n`: lays this frame's HUD over its upscaled eye (at that eye's place from
/// [`crate::hud::draws::eye_rects`]) and gives it to the publisher. A frame with no 3D view goes
/// out flat instead: `back` (the back buffer: a movie, a loading screen) at the eye size, with
/// whatever HUD it drew over the whole of it.
#[allow(clippy::too_many_arguments)]
pub fn publish(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    back: &ID3D11Texture2D,
    n: u64,
    publisher: &mut AlternatePublisher,
    head: &HeadSource,
    report: impl Fn(&mut HeadPose),
) -> Step {
    CAPTURED.store(false, Release);
    let Ok(mut state) = STATE.lock() else { return Step::Failed };
    let state = &mut state.0;
    let hud_drawn = std::mem::take(&mut state.hud_begun);
    let output = state.output.take();
    let Some(eye) = stereo::schedule().and_then(|s| s.shown_eye(n)) else { return Step::Waiting };
    let image = match output {
        Some((shown, image)) if shown == n => image,
        _ if state.viewless >= FLAT_AFTER => return publish_flat(state, device, context, back, n, publisher, head, hud_drawn),
        _ => return Step::Failed,
    };
    let mut image = image;
    crate::research::layer::frame(device, context, back, &image, state.hud.as_ref().filter(|_| hud_drawn).and_then(HudLayer::drawn_on_black), "eye");
    if hud_drawn && let Some(layer) = state.hud.as_mut() {
        let desc = monaka_channel::d3d::texture_desc(&image);
        let rects = crate::hud::draws::eye_rects(&stereo::pending_pose(), desc.Width, desc.Height);
        match layer.composite(context, eye, &image, crate::hud::ui_size::fit_rect(rects[eye])) {
            Ok(with_hud) => image = with_hud.clone(),
            Err(e) => {
                monaka_producer::log_first!(1, "FSR HUD composite failed: {e}");
                state.failures += 1;
            }
        }
    }
    publisher.publish_image(device, context, &image, n, head, report)
}

/// A frame with no 3D view, to the viewer's flat screen (see [`publish`]).
#[allow(clippy::too_many_arguments)]
fn publish_flat(
    state: &mut State,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    back: &ID3D11Texture2D,
    n: u64,
    publisher: &mut AlternatePublisher,
    head: &HeadSource,
    hud_drawn: bool,
) -> Step {
    let (Some(fsr), Some(scaler)) = (state.fsr.as_ref(), state.scaler.as_mut()) else { return Step::Failed };
    let display = fsr.display();
    let mut image = match scaler.run(context, back, display) {
        Ok(image) => image.clone(),
        Err(e) => {
            monaka_producer::log_first!(1, "FSR flat frame failed: {e}");
            state.failures += 1;
            return Step::Failed;
        }
    };
    crate::research::layer::frame(device, context, back, &image, state.hud.as_ref().filter(|_| hud_drawn).and_then(HudLayer::drawn_on_black), "flat");
    if hud_drawn && let Some(layer) = state.hud.as_mut() {
        let desc = monaka_channel::d3d::texture_desc(back);
        match layer.composite(context, 0, &image, crate::hud::ui_size::fit_rect(monaka_warp::fit((desc.Width, desc.Height), display))) {
            Ok(with_hud) => image = with_hud.clone(),
            Err(e) => {
                monaka_producer::log_first!(1, "FSR HUD composite failed: {e}");
                state.failures += 1;
            }
        }
    }
    if state.flat == 0 {
        log!("FSR: a frame with no 3D view (a movie or a loading screen) went to the flat screen");
    }
    state.flat += 1;
    publisher.publish_flat(device, context, &image, n, head)
}

pub fn report() {
    if let Ok(state) = STATE.lock()
        && state.0.fsr.is_some()
    {
        let s = &state.0;
        log!("FSR: upscaled={} busy={} missing_inputs={} flat={} failures={}", s.upscaled, s.busy, s.missing, s.flat, s.failures);
    }
}

/// Drops the upscaler (after the hooks are off; it waits for its GPU work first).
pub fn release() {
    ENABLED.store(false, Release);
    if let Ok(mut state) = STATE.lock() {
        let state = &mut state.0;
        state.output = None;
        state.scaler = None;
        state.hud_views = None;
        state.hud = None;
        state.gbuffer = None;
        state.fsr = None;
    }
}

/// The G-buffer's object motion when `count` render targets with a depth buffer are being bound:
/// the fourth target, R16G16_FLOAT (the camera's motion is not in it).
pub fn motion_target(count: u32, views: *const *mut core::ffi::c_void, depth: bool) -> Option<ID3D11Texture2D> {
    if count < 4 || views.is_null() || !depth {
        return None;
    }
    // SAFETY: the game passes an array of `count` views (each live or null).
    let raw = unsafe { *views.add(3) };
    // SAFETY: a live render target view or null.
    let view = unsafe { ID3D11RenderTargetView::from_raw_borrowed(&raw) }?;
    // SAFETY: COM call on a live view.
    let texture = unsafe { view.GetResource() }.ok()?.cast::<ID3D11Texture2D>().ok()?;
    (monaka_channel::d3d::texture_desc(&texture).Format == DXGI_FORMAT_R16G16_FLOAT).then_some(texture)
}
