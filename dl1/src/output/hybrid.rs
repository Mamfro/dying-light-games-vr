//! DL1's side of alternate-eye rendering with depth (`monaka_stereo::hybrid`): which depth buffer is
//! the scene's (the one bound last before the scene reaches the back buffer; probed 2026-10-05: a
//! 3840x2160 D32S8 texture, every frame), when a frame's world image is complete (the first draw
//! after that copy, before the HUD), and which cameras the frame was rendered with (recorded by
//! the view setup under the present number that chose its eye).

use crate::view::stereo;
use monaka_channel::ChannelName;
use monaka_channel::pose::HeadSource;
use monaka_core::alternate::PresentRing;
use monaka_core::protocol::HeadPose;
use monaka_producer::log;
use monaka_stereo::hybrid::{EyeCameras, Hybrid};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::*};
use windows::Win32::Graphics::Direct3D11::{ID3D11DepthStencilView, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D};
use windows::core::Interface;

struct State {
    hybrid: Option<Hybrid>,
    cameras: PresentRing<EyeCameras>,
}

struct Cell(State);
// SAFETY: the D3D objects inside are used under the mutex by the game's render thread, or by the
// stop after the hooks are off.
unsafe impl Send for Cell {}

static STATE: Mutex<Cell> = Mutex::new(Cell(State { hybrid: None, cameras: PresentRing::new() }));
static ENABLED: AtomicBool = AtomicBool::new(false);
static LAST_DEPTH: AtomicUsize = AtomicUsize::new(0);
/// The scene depth of the latest scene copy, and the present that shows it.
struct HeldDepth(Option<(u64, ID3D11Texture2D)>);
// SAFETY: the texture is only cloned under the mutex; D3D11 resources are free-threaded.
unsafe impl Send for HeldDepth {}
static SCENE_DEPTH: Mutex<HeldDepth> = Mutex::new(HeldDepth(None));
static CAPTURED: AtomicBool = AtomicBool::new(false);

pub fn install(device: &ID3D11Device, channel: ChannelName, hud_layer: bool) -> Result<(), String> {
    let mut hybrid = Hybrid::new(device, channel).map_err(|e| format!("alternate-eye with depth: {e}"))?;
    if hud_layer {
        hybrid.enable_hud(device).map_err(|e| format!("HUD layer: {e}"))?;
    }
    STATE.lock().unwrap_or_else(|e| e.into_inner()).0.hybrid = Some(hybrid);
    HUD_LAYER.store(hud_layer, Release);
    ENABLED.store(true, Release);
    Ok(())
}

static HUD_LAYER: AtomicBool = AtomicBool::new(false);

/// Whether HUD draws go into the HUD layer (laid over both eyes) instead of the back buffer.
pub fn hud_layer() -> bool {
    enabled() && HUD_LAYER.load(Acquire)
}

/// The black and white HUD targets of this frame (each HUD draw goes into both).
pub fn hud_targets(context: &ID3D11DeviceContext, width: u32, height: u32) -> Option<[ID3D11RenderTargetView; 2]> {
    STATE.lock().ok()?.0.hybrid.as_mut()?.hud_targets(context, width, height)
}

pub fn enabled() -> bool {
    ENABLED.load(Acquire)
}

/// Whether this frame still has to be captured (checked before any work on the hot draw path).
pub fn needs_capture() -> bool {
    enabled() && !CAPTURED.load(Acquire)
}

/// The view setup wrote the cameras of the frame whose eye was chosen after present `chosen_at`.
pub fn record_cameras(chosen_at: u64, cameras: EyeCameras) {
    if let Ok(mut state) = STATE.lock() {
        state.0.cameras.put(chosen_at, cameras);
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
    if let (Some(texture), Some(n)) = (texture, stereo::drawing_present())
        && let Ok(mut held) = SCENE_DEPTH.lock()
    {
        held.0 = Some((n, texture));
    }
}

/// A present: depth bound before it belongs to the frame it showed.
pub fn frame_end() {
    LAST_DEPTH.store(0, Relaxed);
}

/// The scene depth of the frame shown at present `n` (tracked while this mode or FSR runs).
pub fn scene_depth(n: u64) -> Option<ID3D11Texture2D> {
    SCENE_DEPTH.lock().ok()?.0.as_ref().filter(|(at, _)| *at == n).map(|(_, texture)| texture.clone())
}

/// Keeps the frame shown at present `n` (its world image is `color` now) as its eye's latest real
/// frame, once per frame.
pub fn capture(context: &ID3D11DeviceContext, color: &ID3D11Texture2D, n: u64) {
    if !enabled() || CAPTURED.swap(true, AcqRel) {
        return;
    }
    let Some(schedule) = stereo::schedule() else { return };
    let eye = schedule.eye_at(n);
    let chosen_at = schedule.chosen_at(n);
    let Some(depth) = scene_depth(n) else { return };
    let Ok(mut state) = STATE.lock() else { return };
    let State { hybrid, cameras } = &mut state.0;
    let Some(cameras) = cameras.get(chosen_at) else { return };
    if eye != monaka_core::alternate::Schedule::camera_eye(chosen_at) {
        return;
    }
    if let Some(hybrid) = hybrid.as_mut() {
        hybrid.capture(context, eye, color, &depth, &cameras);
    }
    static DIAGNOSED: AtomicBool = AtomicBool::new(false);
    if !DIAGNOSED.swap(true, Relaxed) {
        diagnose_depth(context, &depth, &cameras);
    }
}

/// Once per run: the depth buffer's actual values (sky at the top, ground at the bottom) against
/// the projection the mapping was built from, to tell standard from reversed depth.
fn diagnose_depth(context: &ID3D11DeviceContext, depth: &ID3D11Texture2D, cameras: &EyeCameras) {
    let format = monaka_channel::d3d::texture_desc(depth).Format;
    // SAFETY: COM call on the game's live context.
    let Ok(device) = (unsafe { context.GetDevice() }) else { return };
    // A staging copy on the game's render thread (stalls; once per run). Each texel starts with its
    // 32-bit depth (D32S8: 8 bytes, the stencil after it).
    let Ok((w, h, texel, texels)) = monaka_channel::d3d::read_texels(&device, context, depth) else { return };
    if texel < 4 {
        return;
    }
    let value = |x: u32, y: u32| {
        let at = (y * w + x) as usize * texel;
        f32::from_le_bytes([texels[at], texels[at + 1], texels[at + 2], texels[at + 3]])
    };
    let samples = [(w / 2, 2), (w / 2, h / 4), (w / 2, h / 2), (w / 2, 3 * h / 4), (w / 2, h - 3)];
    let values: Vec<String> = samples.iter().map(|&(x, y)| format!("({x},{y})={:.6}", value(x, y))).collect();
    let p = &cameras.projection[0];
    log!(
        "depth diagnosis: format {} values {}; projection P10 {} P11 {} P14 {} P15 {}; mapping a {} b {}",
        format.0,
        values.join(" "),
        p[10],
        p[11],
        p[14],
        p[15],
        cameras.mapping[0].a,
        cameras.mapping[0].b
    );
}

/// At present: publishes this frame's pair, the HUD layer at `hud_rects` in each eye. Returns
/// whether one went out.
pub fn publish(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    final_image: &ID3D11Texture2D,
    head: &HeadSource,
    hud_rects: [[f32; 4]; 2],
    report: impl Fn(&mut HeadPose),
) -> bool {
    CAPTURED.store(false, Release);
    let Ok(mut state) = STATE.lock() else { return false };
    state.0.hybrid.as_mut().is_some_and(|h| h.publish(device, context, final_image, head, hud_rects, report))
}

pub fn report() {
    if let Ok(state) = STATE.lock()
        && let Some(hybrid) = &state.0.hybrid
    {
        log!("alternate-eye with depth: pairs={} failures={}", hybrid.pairs(), hybrid.failures());
    }
}

pub fn release() {
    ENABLED.store(false, Release);
    if let Ok(mut state) = STATE.lock() {
        state.0.hybrid = None;
    }
    if let Ok(mut held) = SCENE_DEPTH.lock() {
        held.0 = None;
    }
}
