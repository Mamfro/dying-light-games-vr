//! Depth-stereo (`stereo=depth`), what only DL2 has: one centre render per frame with both
//! eyes' field of view, both eyes made from it and its depth (`monaka_warp::warp12::DepthWarp`),
//! published through the same-frame loop's pair sink (`monaka_stereo::same_frame::d3d12::sink`).
//! The depth and the game's UI layer are copied on the game's command list when it tags them for
//! DLSS (`crate::providers::streamline`); the HUD is placed in the centre frame's frustum, the
//! warp then moves each world marker by its own disparity. A frame without a UI layer once one has
//! been seen is a menu, published flat.

use crate::config::{self, debug};
use crate::providers::eye_site::local;
use crate::providers::hud::{self, LAST_UI_PRESENT, UI_SEEN, in_menu};
use crate::providers::{present, streamline};
use monaka_channel::d3d12;
use monaka_core::camera::Frustum;
use monaka_core::hud::{Placement, Rect};
use monaka_core::sync::Locked;
use monaka_producer::log;
use monaka_stereo::same_frame::{PAIRS, d3d12::sink};
use monaka_stereo::pairs12::Step;
use monaka_warp::warp12::{DepthWarp, Params};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use windows::Win32::Graphics::Direct3D12::{D3D12_RESOURCE_DESC, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_PRESENT, D3D12_RESOURCE_STATES, ID3D12GraphicsCommandList, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM;

static WARP: Mutex<Option<DepthWarp>> = Mutex::new(None);
static WARP_FAILED: AtomicBool = AtomicBool::new(false);
pub static UI_COPIES: AtomicU64 = AtomicU64::new(0);
pub static DEPTH_COPIES: AtomicU64 = AtomicU64::new(0);
/// Frames whose UI layer was made from the game's HUD draws (`eng_chr::providers::hudlayer`).
pub static OWN_LAYER_FRAMES: AtomicU64 = AtomicU64::new(0);
static FLAT_MENU_FRAMES: AtomicU64 = AtomicU64::new(0);

/// The game draws the player's arms and weapon 0.2-0.3 m from the camera; at that disparity they
/// cannot be fused. Nothing is shifted further than an object at this distance would be.
const NEAREST_COMFORT: f32 = 0.5;

fn with_warp<R>(f: impl FnOnce(&mut DepthWarp) -> R) -> Option<R> {
    let mut warp = WARP.locked();
    if warp.is_none() && !WARP_FAILED.load(Ordering::Acquire) {
        let made = present::game_queue()
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
    if eng_chr::providers::hudlayer::redirecting() {
        return;
    }
    if let Some(Ok(())) = with_warp(|warp| warp.copy_ui(list, ui, state)) {
        UI_COPIES.fetch_add(1, Ordering::Relaxed);
        LAST_UI_PRESENT.store(PAIRS.presents(), Ordering::Relaxed);
        UI_SEEN.store(true, Ordering::Release);
    }
}

/// Depth stereo's centre frame (`buffer`, the back buffer in PRESENT) at its present: both eyes made
/// from it and its depth and published, or (a menu) the frame flat. Whether it was published.
pub fn present(buffer: &ID3D12Resource) -> bool {
    const PRESENT: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_PRESENT;
    const COMMON: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_COMMON;
    // SAFETY: reads a descriptor.
    let desc = unsafe { buffer.GetDesc() };
    let presents = PAIRS.presents();
    // No UI layer tagged by the game this frame (it tags one only in gameplay with its DLSS Frame
    // Generation on): the HUD layer made from the game's own HUD draws stands in
    // (`eng_chr::providers::hudlayer`). Without that too, a frame shortly after a tagged one is a
    // menu or loading screen (the game stops tagging there), shown flat. The layer is taken (and
    // given back below) every frame so it is cleared frame by frame and ready the moment the game's
    // tag stops.
    let tag_gap = presents.saturating_sub(LAST_UI_PRESENT.load(Ordering::Relaxed));
    let tagged = UI_SEEN.load(Ordering::Acquire) && tag_gap <= 2;
    let own_layer = present::game_queue().and_then(|queue| eng_chr::providers::hudlayer::take_layer(&queue, (desc.Width as u32, desc.Height)));
    let own_used = own_layer.is_some() && (!tagged || eng_chr::providers::hudlayer::redirecting());
    if let (Some(layer), true) = (&own_layer, own_used) {
        with_warp(|warp| warp.use_layer(layer));
        if OWN_LAYER_FRAMES.fetch_add(1, Ordering::Relaxed) == 0 {
            log!(
                "depth stereo: the HUD layer comes from the game's own HUD draws{}",
                if eng_chr::providers::hudlayer::redirecting() { ", kept out of the frame" } else { " (the game tags no UI layer: its frame generation is off)" }
            );
        }
    } else if !tagged && in_menu() {
        // The plain frame, flat.
        let step = sink([(buffer, PRESENT), (buffer, PRESENT)], None, None);
        if step == Step::Published {
            FLAT_MENU_FRAMES.fetch_add(1, Ordering::Relaxed);
        }
        return step == Step::Published;
    } else if !tagged && presents > 900 {
        monaka_producer::log_first!(1, "depth stereo: no UI layer from the game (frame {presents}) and none of our own; the HUD is warped with the world");
    }
    // Both eyes from this frame and the depth copied when DLSS tagged it.
    let (scale, near) = (streamline::PROJECTION_SCALE.load(), streamline::NEAR_PLANE.load());
    if !(scale > 0.0 && near > 0.0 && streamline::DEPTH_INVERTED.load(Ordering::Acquire)) {
        monaka_producer::log_first!(1, "depth stereo needs reverse-Z camera constants from DLSS; none yet");
        if let Some(queue) = present::game_queue() {
            eng_chr::providers::hudlayer::return_layer(&queue);
        }
        return false;
    }
    let Some(queue) = present::game_queue() else { return false };
    let (pair_head, pair_fov) = local(|l| (l.pair_head.get(), l.pair_fov.get()));
    let config = config::get();
    // Pixels per unit of depth (reverse-Z depth is the nearness).
    let disparity = monaka_core::depth::disparity_scale(scale, desc.Width as u32, pair_head.ipd, near);
    // Both eyes come from the centre view: the HUD is placed in its frustum.
    let centre = Frustum::from_fov(pair_fov);
    let mut params = Params::depth_stereo(
        disparity,
        monaka_core::depth::max_shift(scale, desc.Width as u32, pair_head.ipd, NEAREST_COMFORT).max(1.0) as u32,
        centre_hud_rects(&desc, centre, pair_head.ipd),
        hud::aim((desc.Width as u32, desc.Height)),
        monaka_stereo::hudover12::CROSS_RADIUS * desc.Height as f32,
    );
    params.ui_out_of_frame = own_used && eng_chr::providers::hudlayer::redirecting();
    if config.hud.world
        && let Some(Some(ui)) = with_warp(|warp| warp.ui().cloned())
    {
        // The pieces onto the hands; the warp leaves them out of the HUD over the eyes.
        for (slot, rect) in params.pieces.iter_mut().zip(hud::publish_pieces(&queue, &ui)) {
            *slot = rect;
        }
        params.markers = hud::world_markers(&desc, centre, pair_head.ipd);
    }
    let eyes = with_warp(|warp| match warp.run(&queue, buffer, PRESENT, &params) {
        Ok(true) => Some((warp.eye(0).cloned()?, warp.eye(1).cloned()?)),
        Ok(false) => None,
        Err(e) => {
            monaka_producer::log_first!(1, "depth warp not run: {e}");
            None
        }
    });
    // The layer cleared and a render target again for the next frame (the warp's reads on it
    // are queued already); the first return readies a new layer.
    eng_chr::providers::hudlayer::return_layer(&queue);
    let Some(Some((left, right))) = eyes else { return false };
    if let Some(Some(ui)) = with_warp(|warp| warp.ui().cloned()) {
        crate::research::markers::ui(&queue, &ui);
    }
    let mut pose = pair_head;
    pose.fov = [pair_fov, pair_fov];
    sink([(&left, COMMON), (&right, COMMON)], Some([pose, pose]), None) == Step::Published
}

/// A check (`HUDLESS_LEFT`): a frame of no eye without a pair lately goes out flat with the game's
/// UI layer (the HUD alone) as its left eye, the finished frame as its right. `Some` when it went.
pub fn hudless_left(buffer: &ID3D12Resource) -> Option<bool> {
    if !config::debug(debug::HUDLESS_LEFT) || PAIRS.recent_pair() {
        return None;
    }
    // SAFETY: reads a descriptor.
    let desc = unsafe { buffer.GetDesc() };
    if desc.Format != DXGI_FORMAT_R8G8B8A8_UNORM {
        return None;
    }
    let ui = WARP.locked().as_ref().and_then(|w| w.ui().cloned())?;
    // SAFETY: reads a descriptor.
    let d = unsafe { ui.GetDesc() };
    if d.Width != desc.Width || d.Height != desc.Height {
        return None;
    }
    sink([(&ui, D3D12_RESOURCE_STATE_COMMON), (buffer, D3D12_RESOURCE_STATE_PRESENT)], None, None);
    Some(false)
}

/// Depth stereo: where the HUD goes in each eye made from a centre frame of `desc` drawn with
/// `centre` (the whole frame without one): at the HUD distance and size, its disparity from the
/// eyes' separation `ipd`. Its own placement, not `HudLayer for Dl2`'s: no eye presents here, both
/// come from the one frame (the warp then moves each marker by its own disparity).
fn centre_hud_rects(desc: &D3D12_RESOURCE_DESC, centre: Option<Frustum>, ipd: f32) -> [[f32; 4]; 2] {
    let config = config::get();
    let image = Rect::sized(desc.Width as f32, desc.Height as f32);
    std::array::from_fn(|eye| match centre {
        Some(frustum) => Placement::new(frustum, eye, ipd, config.hud.distance).rect(image, image.aspect(), config.hud.scale).array(),
        None => image.array(),
    })
}

/// Lets the warp go once the GPU is done with it (leaked if it never is).
pub fn close() {
    if let Some(warp) = WARP.locked().take()
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
