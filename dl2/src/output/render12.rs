//! The D3D12 renderer side (rd3d12): eye images from the back buffer at each present request,
//! published over the fence channel on the game's own queue, and per-eye DLSS.
//!
//! - Same-frame stereo: the left eye is kept ([`PairPublisher12::hold`]) at its present; the
//!   right eye's present publishes the pair.
//! - Alternate-eye: the same, one eye per frame.
//! - Depth stereo: both eyes made from the centre frame and its depth ([`DepthWarp`]); a frame
//!   without a UI layer once one has been seen is a menu, published flat.
//! - Anything else (menus, loading, no head pose) goes out mono.

use crate::config::{self, debug};
use crate::engine::{self, Packet12Fn, PresentRequest12Fn};
use crate::view::scene::{self, LAST_STEREO_TICK, RENDER_EYE, local};
use crate::output::warp12::{DepthWarp, Params};
use crate::{output::dlss, game, view::head, output::streamline};
use monaka_channel::d3d12;
use monaka_channel::ChannelName;
use monaka_core::camera::Frustum;
use monaka_core::hud::{Placement, Rect};
use monaka_core::protocol::HeadPose;
use monaka_stereo::pairs12::{PairPublisher12, Step};
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::log;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_RESOURCE_DESC, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_PRESENT, D3D12_RESOURCE_STATES, ID3D12CommandQueue, ID3D12GraphicsCommandList, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGISwapChain3;
use windows::core::Interface;

pub static PRESENT_REQUEST: Original<PresentRequest12Fn> = Original::new();
pub static DLSS_EVALUATE: Original<Packet12Fn> = Original::new();
pub static DLSS_CONSTANTS: Original<Packet12Fn> = Original::new();

static CHANNEL_NAME: OnceLock<ChannelName> = OnceLock::new();
static CHANNEL: Mutex<Option<PairPublisher12>> = Mutex::new(None);
/// The pair whose left eye is held.
static HELD_PAIR: AtomicU64 = AtomicU64::new(0);
static WARP: Mutex<Option<DepthWarp>> = Mutex::new(None);
static WARP_FAILED: AtomicBool = AtomicBool::new(false);
static DLSS_LOCK: Mutex<()> = Mutex::new(());
/// The game separates its UI only during gameplay; menus, maps and loading screens draw it onto
/// the scene. Once a UI layer has been seen, a frame without one is a menu, shown flat.
pub static UI_SEEN: AtomicBool = AtomicBool::new(false);
pub static LAST_UI_PRESENT: AtomicU64 = AtomicU64::new(0);

/// The game tagged its UI layer (it does only in gameplay).
pub fn ui_tagged() {
    LAST_UI_PRESENT.store(crate::PRESENTS.load(Ordering::Relaxed), Ordering::Relaxed);
    UI_SEEN.store(true, Ordering::Release);
}

/// A menu, map, inventory or loading screen: a UI layer has been seen, but none for a few presents.
pub fn in_menu() -> bool {
    UI_SEEN.load(Ordering::Acquire) && crate::PRESENTS.load(Ordering::Relaxed).saturating_sub(LAST_UI_PRESENT.load(Ordering::Relaxed)) > 4
}
pub static UI_COPIES: AtomicU64 = AtomicU64::new(0);
pub static DEPTH_COPIES: AtomicU64 = AtomicU64::new(0);
/// Frames whose UI layer was made from the game's HUD draws (`eng_chr::hudlayer`).
pub static OWN_LAYER_FRAMES: AtomicU64 = AtomicU64::new(0);
static FLAT_MENU_FRAMES: AtomicU64 = AtomicU64::new(0);

/// The game draws the player's arms and weapon 0.2-0.3 m from the camera; at that disparity they
/// cannot be fused. Nothing is shifted further than an object at this distance would be.
const NEAREST_COMFORT: f32 = 0.5;

pub fn set_channel(name: ChannelName) {
    *CHANNEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(PairPublisher12::new(name.clone()));
    let _ = CHANNEL_NAME.set(name);
}

/// The game's direct queue (a Streamline proxy when Streamline is loaded).
pub fn game_queue() -> Option<ID3D12CommandQueue> {
    engine::RD3D12.game_queue(game::get().renderer12?)
}

fn with_warp<R>(f: impl FnOnce(&mut DepthWarp) -> R) -> Option<R> {
    let mut warp = WARP.lock().unwrap_or_else(|e| e.into_inner());
    if warp.is_none() && !WARP_FAILED.load(Ordering::Acquire) {
        let made = game_queue()
            .ok_or_else(|| "no game queue".to_owned())
            .and_then(|queue| d3d12::device_of(&queue).map_err(|e| e.to_string()))
            .and_then(|device| DepthWarp::new(&device));
        match made {
            Ok(made) => {
                log!("depth warp ready");
                *warp = Some(made);
            }
            Err(why) => {
                WARP_FAILED.store(true, Ordering::Release);
                log!("depth warp unavailable: {why}");
            }
        }
    }
    warp.as_mut().map(f)
}

/// The depth, recorded on the game's command list when it tags it for DLSS.
pub fn copy_depth(list: &ID3D12GraphicsCommandList, depth: &ID3D12Resource, state: D3D12_RESOURCE_STATES) {
    match with_warp(|warp| warp.copy_depth(list, depth, state)) {
        Some(Ok(())) => {
            DEPTH_COPIES.fetch_add(1, Ordering::Relaxed);
        }
        Some(Err(e)) => monaka_producer::log_first!(1, "depth copy failed: {e}"),
        None => {}
    }
}

/// The UI layer, recorded on the game's command list when it tags it for frame generation.
pub fn copy_ui(list: &ID3D12GraphicsCommandList, ui: &ID3D12Resource, state: D3D12_RESOURCE_STATES) {
    // With the HUD kept out of the frame, the layer of our own is the UI; the game's is not copied.
    if eng_chr::hudlayer::redirecting() {
        return;
    }
    if let Some(Ok(())) = with_warp(|warp| warp.copy_ui(list, ui, state)) {
        UI_COPIES.fetch_add(1, Ordering::Relaxed);
        LAST_UI_PRESENT.store(crate::PRESENTS.load(Ordering::Relaxed), Ordering::Relaxed);
        UI_SEEN.store(true, Ordering::Release);
    }
}

fn published(step: Step, eye_copy: bool) {
    let ok = step == Step::Published;
    if ok && eye_copy {
        local(|l| l.eye_copies.set(l.eye_copies.get() + 1));
    }
    crate::count(ok);
}

/// No pose adjustment: the eyes go out with the pose given.
fn as_is(_: &mut HeadPose) {}

fn capture(swap: &IDXGISwapChain3) {
    crate::PRESENTS.fetch_add(1, Ordering::Relaxed);
    // SAFETY: COM calls on the game's live swapchain.
    let Ok(buffer) = (unsafe { swap.GetBuffer::<ID3D12Resource>(swap.GetCurrentBackBufferIndex()) }) else { return };
    let mut guard = CHANNEL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(channel) = guard.as_mut().filter(|c| !c.broken()) else { return };
    if !channel.made() {
        // The headset size first, when asked for: the channel takes the first frame's size.
        // SAFETY: reads a descriptor.
        let desc = unsafe { buffer.GetDesc() };
        if !crate::output::video::at_headset_size(desc.Width as u32, desc.Height) {
            return;
        }
        let Some(queue) = game_queue() else {
            monaka_producer::log_first!(3, "game queue unavailable; not publishing yet");
            return;
        };
        if !channel.ready_for(&buffer, &queue) {
            return;
        }
        log!("D3D12 fence channel ready on queue {:?}", queue.as_raw());
    }
    let eye = local(|l| l.dispatch_eye.get());
    if crate::output::framegen::enabled() && (eye == 1 || eye == 2) {
        // Frame generation publishes from its own path (and a pacing thread) through
        // [`publish_pair`], which takes this lock: let go of it first (held, the right eye's
        // publish on this thread waits on itself and the game freezes).
        drop(guard);
        return frame_generation(eye, &buffer);
    }
    // SAFETY: reads a descriptor.
    let desc = unsafe { buffer.GetDesc() };
    let tick = game::tick();
    const PRESENT: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_PRESENT;
    const COMMON: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_COMMON;
    let presents = crate::PRESENTS.load(Ordering::Relaxed);
    // No UI layer tagged by the game this frame (it tags one only in gameplay with its DLSS Frame
    // Generation on): the HUD layer made from the game's own HUD draws stands in
    // (`eng_chr::hudlayer`). Without that too, a frame shortly after a tagged one is a menu or
    // loading screen (the game stops tagging there), shown flat.
    // The layer is taken (and given back below) every frame so it is cleared frame by frame
    // and ready the moment the game's tag stops.
    let tag_gap = presents.saturating_sub(LAST_UI_PRESENT.load(Ordering::Relaxed));
    let tagged = UI_SEEN.load(Ordering::Acquire) && tag_gap <= 2;
    let own_layer = if (1..=3).contains(&eye) { game_queue().and_then(|queue| eng_chr::hudlayer::take_layer(&queue, (desc.Width as u32, desc.Height))) } else { None };
    let own_used = own_layer.is_some() && (!tagged || eng_chr::hudlayer::redirecting());
    if let (Some(layer), true) = (&own_layer, own_used) {
        with_warp(|warp| warp.use_layer(layer));
        if OWN_LAYER_FRAMES.fetch_add(1, Ordering::Relaxed) == 0 {
            log!(
                "depth stereo: the HUD layer comes from the game's own HUD draws{}",
                if eng_chr::hudlayer::redirecting() { ", kept out of the frame" } else { " (the game tags no UI layer: its frame generation is off)" }
            );
        }
    } else if eye == 3 && !tagged && in_menu() {
        // The plain frame, flat, no pose record.
        let step = channel.publish_unposed([(&buffer, PRESENT), (&buffer, PRESENT)]);
        if step == Step::Published {
            FLAT_MENU_FRAMES.fetch_add(1, Ordering::Relaxed);
        }
        return published(step, true);
    } else if eye == 3 && !tagged && presents > 900 {
        monaka_producer::log_first!(1, "depth stereo: no UI layer from the game (frame {presents}) and none of our own; the HUD is warped with the world");
    }
    if eye == 3 {
        // Depth stereo: both eyes from this frame and the depth copied when DLSS tagged it.
        let (scale, near) = (streamline::PROJECTION_SCALE.load(), streamline::NEAR_PLANE.load());
        if !(scale > 0.0 && near > 0.0 && streamline::DEPTH_INVERTED.load(Ordering::Acquire)) {
            monaka_producer::log_first!(1, "depth stereo needs reverse-Z camera constants from DLSS; none yet");
            if let Some(queue) = game_queue() {
                eng_chr::hudlayer::return_layer(&queue);
            }
            return;
        }
        let Some(queue) = game_queue() else { return };
        let (pair_head, pair_fov, aim) = local(|l| (l.pair_head.get(), l.pair_fov.get(), l.pair_aim.get()));
        let config = config::get();
        // Pixels per unit of depth (reverse-Z depth is the nearness).
        let disparity = monaka_core::depth::disparity_scale(scale, desc.Width as u32, pair_head.ipd, near);
        let aim = aim.map_or([0.0, 0.0], |a| [a[0] * 0.5 * desc.Width as f32, -a[1] * 0.5 * desc.Height as f32]);
        // Both eyes come from the centre view: the HUD is placed in its frustum.
        let centre = Frustum::from_fov(pair_fov);
        let mut params = Params::depth_stereo(
            disparity,
            monaka_core::depth::max_shift(scale, desc.Width as u32, pair_head.ipd, NEAREST_COMFORT).max(1.0) as u32,
            hud_rects(&desc, [centre, centre], pair_head.ipd),
            aim,
            0.03 * desc.Height as f32,
        );
        params.ui_out_of_frame = own_used && eng_chr::hudlayer::redirecting();
        if config.hud.world
            && let Some(name) = CHANNEL_NAME.get()
            && let Some(Some(ui)) = with_warp(|warp| warp.ui().cloned())
        {
            // The pieces onto the hands; the warp leaves them out of the HUD over the eyes.
            for (slot, rect) in params.pieces.iter_mut().zip(eng_chr::panels::publish(&queue, &ui, name, tick)) {
                *slot = rect;
            }
            params.markers = world_markers(&desc, centre, pair_head.ipd);
        }
        let eyes = with_warp(|warp| match warp.run(&queue, &buffer, PRESENT, &params) {
            Ok(true) => Some((warp.eye(0).cloned()?, warp.eye(1).cloned()?)),
            Ok(false) => None,
            Err(e) => {
                monaka_producer::log_first!(1, "depth warp not run: {e}");
                None
            }
        });
        // The layer cleared and a render target again for the next frame (the warp's reads on it
        // are queued already); the first return readies a new layer.
        eng_chr::hudlayer::return_layer(&queue);
        let Some(Some((left, right))) = eyes else { return };
        if let Some(Some(ui)) = with_warp(|warp| warp.ui().cloned()) {
            crate::research::markers::ui(&queue, &ui);
        }
        let mut pose = pair_head;
        pose.fov = [pair_fov, pair_fov];
        return published(channel.publish([(&left, COMMON), (&right, COMMON)], [pose, pose], &head::HEAD, as_is), true);
    }
    if eye == 1 || eye == 2 {
        // A rendered eye (alternate-eye or same-frame stereo): with the dynamic HUD, the HUD
        // layer (kept out of the frame) laid over it at HUD distance and its pieces to the hands.
        let shown = match &own_layer {
            Some(layer) if config::get().hud.world && eng_chr::hudlayer::redirecting() => overlay_eye(&buffer, &desc, layer, eye as usize - 1, tick),
            _ => None,
        };
        // Given back every frame (the first return readies a new layer; a layer not taken this
        // frame is left as it is).
        if let Some(queue) = game_queue() {
            eng_chr::hudlayer::return_layer(&queue);
        }
        let (shown, state) = shown.map_or((buffer.clone(), PRESENT), |eye| (eye, COMMON));
        if eye == 1 {
            if channel.hold(&shown, state) {
                HELD_PAIR.store(local(|l| l.dispatch_pair.get()), Ordering::Release);
                local(|l| l.eye_copies.set(l.eye_copies.get() + 1));
            }
            return;
        }
        if HELD_PAIR.load(Ordering::Acquire) != local(|l| l.dispatch_pair.get()) {
            return;
        }
        let Some(held) = channel.held() else { return };
        let pair_head = local(|l| l.pair_head.get());
        return published(channel.publish([(&held, COMMON), (&shown, state)], [pair_head, pair_head], &head::HEAD, as_is), true);
    }
    if tick.saturating_sub(LAST_STEREO_TICK.load(Ordering::Acquire)) <= 250 {
        // Between pairs; the next pair publishes.
        return;
    }
    // Debug: left = the HUD-less colour, right = the finished frame.
    if config::debug(debug::HUDLESS_LEFT) && desc.Format == DXGI_FORMAT_R8G8B8A8_UNORM {
        let ui = WARP.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|w| w.ui().cloned());
        if let Some(ui) = ui {
            // SAFETY: reads a descriptor.
            let d = unsafe { ui.GetDesc() };
            if d.Width == desc.Width && d.Height == desc.Height {
                return published(channel.publish_unposed([(&ui, COMMON), (&buffer, PRESENT)]), false);
            }
        }
    }
    published(channel.publish_unposed([(&buffer, PRESENT), (&buffer, PRESENT)]), false);
}

/// Where the HUD goes in each eye of a frame of `desc` whose eyes are drawn with `frustums`
/// (the whole frame for an eye without one): at the HUD distance and size, its disparity from
/// the eyes' separation `ipd`.
fn hud_rects(desc: &D3D12_RESOURCE_DESC, frustums: [Option<Frustum>; 2], ipd: f32) -> [[f32; 4]; 2] {
    let config = config::get();
    let image = Rect::sized(desc.Width as f32, desc.Height as f32);
    std::array::from_fn(|eye| match frustums[eye] {
        Some(frustum) => Placement::new(frustum, eye, ipd, config.hud.distance).rect(image, image.aspect(), config.hud.scale).array(),
        None => image.array(),
    })
}

/// The disparity in pixels (half per eye) of a point `distance` metres away in a frame of `desc`
/// drawn with `frustum` (0 without one).
fn disparity_at(desc: &D3D12_RESOURCE_DESC, frustum: Option<Frustum>, ipd: f32, distance: f32) -> f32 {
    frustum.map_or(0.0, |f| Placement::new(f, 0, ipd, distance).shift_pixels(desc.Width as f32))
}

/// A marker's box in the HUD layout, as fractions of its width and height (icon and label).
const MARKER_BOX: [f32; 2] = [0.08, 0.06];

/// The HUD's world markers of the latest tick as the lay-over draws them: each one's box in the
/// frame's pixels (the layout's place in the frame added) and its disparity at its target's
/// distance in a frame drawn with `frustum`. None with `world_markers=0`.
fn world_markers(desc: &D3D12_RESOURCE_DESC, frustum: Option<Frustum>, ipd: f32) -> Vec<crate::output::warp12::Marker> {
    if !config::get().hud.markers {
        return Vec::new();
    }
    let layout = eng_chr::gui::layout();
    let origin = layout.map_or([0.0, 0.0], |l| l.origin);
    let screen = layout.filter(|l| l.screen[0] > 1.0 && l.screen[1] > 1.0).map_or([desc.Width as f32, desc.Height as f32], |l| l.screen);
    let (bw, bh) = (MARKER_BOX[0] * screen[0], MARKER_BOX[1] * screen[1]);
    crate::hud::markers::latest(crate::PRESENTS.load(Ordering::Relaxed))
        .into_iter()
        .map(|m| crate::output::warp12::Marker { rect: [origin[0] + m.screen[0] - bw * 0.5, origin[1] + m.screen[1] - bh * 0.5, bw, bh], shift: disparity_at(desc, frustum, ipd, m.distance) })
        .collect()
}

/// How the HUD `layer` (kept out of the frame) is laid over a rendered eye's frame of `desc`:
/// at HUD distance with its disparity, the crosshair at the aim point, the pieces left out; the
/// left eye (`left`) cuts the pieces out onto the hands' panels first.
pub fn hud_params(queue: &ID3D12CommandQueue, desc: &D3D12_RESOURCE_DESC, layer: &ID3D12Resource, left: bool, tick: u64) -> Params<'static> {
    let (pair_head, aim) = local(|l| (l.pair_head.get(), l.pair_aim.get()));
    let aim = aim.map_or([0.0, 0.0], |a| [a[0] * 0.5 * desc.Width as f32, -a[1] * 0.5 * desc.Height as f32]);
    // Each eye was drawn with its own frustum (centred when the run asks for that).
    let frustums = [Frustum::from_fov(pair_head.fov[0]), Frustum::from_fov(pair_head.fov[1])];
    let mut params = Params::depth_stereo(0.0, 0, hud_rects(desc, frustums, pair_head.ipd), aim, 0.03 * desc.Height as f32);
    params.ui_out_of_frame = true;
    let mut pieces = LAST_PIECES.lock().unwrap_or_else(|e| e.into_inner());
    if left && let Some(name) = CHANNEL_NAME.get() {
        *pieces = [[0.0; 4]; 5];
        for (slot, rect) in pieces.iter_mut().zip(eng_chr::panels::publish(queue, layer, name, tick)) {
            *slot = rect;
        }
    }
    params.pieces = *pieces;
    params.markers = world_markers(desc, frustums[0], pair_head.ipd);
    params
}

/// The rects of the pieces last cut out (the left eye cuts them; the right eye leaves out the same).
static LAST_PIECES: Mutex<[[f32; 4]; 5]> = Mutex::new([[0.0; 4]; 5]);
/// Rendered eyes with the HUD laid over so far.
pub static OVERLAID_EYES: AtomicU64 = AtomicU64::new(0);

/// A rendered eye frame (`eye` 0 left, 1 right) with the HUD `layer` laid over it at HUD distance,
/// the pieces cut out onto the hands' panels (on the left eye's frame): the warp's eye texture, in
/// COMMON. `None`: the lay-over could not run (the plain frame goes out).
fn overlay_eye(buffer: &ID3D12Resource, desc: &D3D12_RESOURCE_DESC, layer: &ID3D12Resource, eye: usize, tick: u64) -> Option<ID3D12Resource> {
    let queue = game_queue()?;
    with_warp(|warp| warp.use_layer(layer));
    let params = hud_params(&queue, desc, layer, eye == 0, tick);
    let shown = with_warp(|warp| match warp.overlay(&queue, buffer, D3D12_RESOURCE_STATE_PRESENT, &params, eye) {
        Ok(true) => warp.eye(eye).cloned(),
        Ok(false) => None,
        Err(e) => {
            monaka_producer::log_first!(1, "HUD lay-over not run: {e}");
            None
        }
    })?;
    if shown.is_some() && OVERLAID_EYES.fetch_add(1, Ordering::Relaxed) == 0 {
        log!("rendered eyes: the HUD layer (the game's HUD draws, kept out of the frame) is laid over each eye at {} m; the pieces go to the hands", config::get().hud.distance);
    }
    shown
}

/// Same-frame stereo with frame generation: each eye goes to its generator at its present, with
/// the HUD layer of the frame (kept out of it) to lay over the real and the generated frames.
fn frame_generation(eye: u32, buffer: &ID3D12Resource) {
    let Some(queue) = game_queue() else { return };
    // SAFETY: reads a descriptor.
    let desc = unsafe { buffer.GetDesc() };
    let layer = eng_chr::hudlayer::take_layer(&queue, (desc.Width as u32, desc.Height));
    let hud = layer
        .as_ref()
        .filter(|_| config::get().hud.world && eng_chr::hudlayer::redirecting())
        .map(|layer| crate::output::framegen::Hud { layer, params: hud_params(&queue, &desc, layer, eye == 1, game::tick()) });
    let outcome = if eye == 1 {
        crate::output::framegen::left(&queue, buffer, hud.as_ref())
    } else {
        crate::output::framegen::right(&queue, buffer, local(|l| l.pair_head.get()), hud.as_ref())
    };
    eng_chr::hudlayer::return_layer(&queue);
    if matches!(outcome, crate::output::framegen::Outcome::Handled) {
        local(|l| l.eye_copies.set(l.eye_copies.get() + 1));
    }
}

/// Publishes a finished pair (both eyes in the common state) with its pose; any thread.
pub fn publish_pair(eyes: &[ID3D12Resource; 2], pose: HeadPose) {
    let mut channel = CHANNEL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(channel) = channel.as_mut() else { return };
    const COMMON: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_COMMON;
    published(channel.publish([(&eyes[0], COMMON), (&eyes[1], COMMON)], [pose, pose], &head::HEAD, as_is), false);
}

pub unsafe extern "system" fn present_request(object: usize) -> usize {
    let _guard = InFlight::enter();
    if scene::publishing() {
        let swap = engine::RD3D12.swapchain(object);
        if swap != 0 && engine::RD3D12.active(object) {
            let raw = swap as *mut c_void;
            // SAFETY: the request's swapchain, live while it presents.
            if let Some(swap) = unsafe { IDXGISwapChain3::from_raw_borrowed(&raw) } {
                capture(swap);
            }
        }
        // The HUD layer's frame (its draws recorded on the game's lists) ends with the present.
        eng_chr::hudlayer::frame_end();
    }
    // SAFETY: the original, with the game's argument.
    unsafe { PRESENT_REQUEST.get()(object) }
}

/// D3D12 DLSS takes (native state, packet). The viewport id lives at (*rd3d12+DLSS_OWNER)+0x50,
/// falling back to the cached copy at +0x30 of the constants state or of the global DLSS state.
unsafe fn scoped(state: usize, packet: usize, original: Packet12Fn, evaluate: bool) -> usize {
    let _guard = InFlight::enter();
    let eye = if config::debug(debug::NATIVE_DLSS_HISTORY) { 0 } else { RENDER_EYE.load(Ordering::Acquire) };
    let r12 = game::get().renderer12.unwrap_or(0);
    let owner = mem::read::<usize>(r12 + engine::DLSS_OWNER_12).unwrap_or(0);
    let cache = if evaluate { mem::read::<usize>(r12 + engine::DLSS_GLOBAL_12).unwrap_or(0) } else { state };
    let viewport = if owner != 0 {
        owner + 0x50
    } else if cache != 0 {
        cache + 0x30
    } else {
        0
    };
    if !(1..=2).contains(&eye) || viewport == 0 {
        // SAFETY: the original, with the renderer's arguments.
        return unsafe { original(state, packet) };
    }
    let _lock = DLSS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cache = (cache != 0).then(|| (cache + 0x30) as *mut u32);
    // SAFETY: the renderer's DLSS state holds the viewport id (and its cached copy) as u32s; the
    // ids are put back after the call, under the lock.
    unsafe { dlss::for_eye(eye, viewport as *mut u32, cache, packet, evaluate, |packet| original(state, packet)) }
}

pub unsafe extern "system" fn dlss_evaluate(state: usize, packet: usize) -> usize {
    // SAFETY: forwards the renderer's arguments.
    unsafe { scoped(state, packet, DLSS_EVALUATE.get(), true) }
}

pub unsafe extern "system" fn dlss_constants(state: usize, packet: usize) -> usize {
    // SAFETY: forwards the renderer's arguments.
    unsafe { scoped(state, packet, DLSS_CONSTANTS.get(), false) }
}

/// Releases the channel and the warp once the GPU is done with them (leaked if it never is).
pub fn close() {
    eng_chr::panels::close();
    if let Some(mut channel) = CHANNEL.lock().unwrap_or_else(|e| e.into_inner()).take() {
        channel.close();
    }
    if let Some(warp) = WARP.lock().unwrap_or_else(|e| e.into_inner()).take()
        && !warp.idle()
    {
        std::mem::forget(warp);
    }
}

pub fn report() -> String {
    format!(
        "UI layers copied={}, depth copies={}, menu frames shown flat={}",
        UI_COPIES.load(Ordering::Relaxed),
        DEPTH_COPIES.load(Ordering::Relaxed),
        FLAT_MENU_FRAMES.load(Ordering::Relaxed)
    )
}
