//! Provider: the HUD layer over each rendered eye, for the same-frame loop
//! (`monaka_stereo::same_frame::PairHud`): the layer of the game's own HUD draws, kept out of the
//! frame (`eng_chr::providers::hudlayer`), placed in that eye's frustum at the HUD's distance and
//! size (`HudLayer for Dl2`, `crate::providers::stereo`), the crosshair at the game's aim point, the
//! pieces on the hands left out (the left eye cuts them out onto the hands' panels), the world
//! markers at their targets' depth. Also: when the game's UI is a menu, from the UI layer it tags
//! in play only ([`in_menu`]); and the HUD pieces for Monaka's dynamic HUD
//! (`monaka_stereo::hud_panels::LayerPieces`, its layer crops), read from the engine's gui tree.

use crate::Dl2;
use crate::config;
use crate::providers::engine;
use crate::providers::eye_site::local;
use monaka_core::camera::Frustum;
use monaka_core::hud::Placement;
use monaka_core::sync::Locked;
use monaka_stereo::hud_panels::{self, HudPieces, LayerPieces, Piece};
use monaka_stereo::providers::HudLayer;
use monaka_stereo::same_frame::{EyeHud, PAIRS, PairHud};
use monaka_warp::warp12::{Marker, Params};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use windows::Win32::Graphics::Direct3D12::{D3D12_RESOURCE_DESC, ID3D12CommandQueue, ID3D12Resource};

/// The HUD layer over each eye of a pair.
pub const PAIR_HUD: PairHud = PairHud { take, done };

/// DL2's pieces ([`engine::PIECES`]).
impl HudPieces for Dl2 {
    const PIECES: &'static [Piece] = &engine::PANELS;
    const MARGIN: f32 = eng_chr::providers::gui::MARGIN;
}

/// Where each piece lies in the HUD layer this frame (the gui tree's boxes, in the frame's pixels).
impl LayerPieces for Dl2 {
    fn boxes(boxes: &mut [Option<[f32; 4]>]) {
        eng_chr::providers::gui::boxes(boxes);
    }
}

// --- The UI layer the game tags ---------------------------------------------------------------------

/// The game separates its UI only during gameplay; menus, maps and loading screens draw it onto
/// the scene. Once a UI layer has been seen, a frame without one is a menu, shown flat.
pub static UI_SEEN: AtomicBool = AtomicBool::new(false);
pub static LAST_UI_PRESENT: AtomicU64 = AtomicU64::new(0);

/// The game tagged its UI layer (it does only in gameplay).
pub fn ui_tagged() {
    LAST_UI_PRESENT.store(PAIRS.presents(), Ordering::Relaxed);
    UI_SEEN.store(true, Ordering::Release);
}

/// A menu, map, inventory or loading screen: a UI layer has been seen, but none for a few presents.
pub fn in_menu() -> bool {
    UI_SEEN.load(Ordering::Acquire) && PAIRS.presents().saturating_sub(LAST_UI_PRESENT.load(Ordering::Relaxed)) > 4
}

// --- Over each eye ------------------------------------------------------------------------------------

/// At eye `eye`'s present: the HUD layer taken (given back at [`done`]), and with the dynamic HUD
/// (the layer kept out of the frame) the features' work on the frame first, then how the layer goes
/// over this eye.
fn take(queue: &ID3D12CommandQueue, buffer: &ID3D12Resource, eye: usize) -> Option<EyeHud> {
    // SAFETY: reads a descriptor.
    let desc = unsafe { buffer.GetDesc() };
    let layer = eng_chr::providers::hudlayer::take_layer(queue, (desc.Width as u32, desc.Height))?;
    if !(config::get().hud.world && eng_chr::providers::hudlayer::redirecting()) {
        return None;
    }
    monaka_stereo::providers::before_hud(queue, buffer);
    let params = hud_params(queue, &desc, &layer, eye == 0);
    Some(EyeHud { layer, params })
}

/// After the eye's images were made: the same layer over the features' own images of the frame,
/// and the layer given back every frame (the first return readies a new layer; a layer not taken
/// this frame is left as it is).
fn done(queue: &ID3D12CommandQueue, hud: Option<&EyeHud>) {
    if let Some(hud) = hud {
        // SAFETY: reads a descriptor (the layer has the frame's size).
        let desc = unsafe { hud.layer.GetDesc() };
        lay_beside(queue, &hud.layer, &desc, &hud.params);
    }
    eng_chr::providers::hudlayer::return_layer(queue);
}

/// The game's aim point from the centre of a frame of `size`, in pixels (x right, y down): the
/// pair's aim in the rendered view (the centre without one).
pub fn aim(size: (u32, u32)) -> [f32; 2] {
    local(|l| l.pair_aim.get()).map_or([0.0, 0.0], |a| [a[0] * 0.5 * size.0 as f32, -a[1] * 0.5 * size.1 as f32])
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
pub fn world_markers(desc: &D3D12_RESOURCE_DESC, frustum: Option<Frustum>, ipd: f32) -> Vec<Marker> {
    if !config::get().hud.markers {
        return Vec::new();
    }
    let layout = eng_chr::providers::gui::layout();
    let origin = layout.map_or([0.0, 0.0], |l| l.origin);
    let screen = layout.filter(|l| l.screen[0] > 1.0 && l.screen[1] > 1.0).map_or([desc.Width as f32, desc.Height as f32], |l| l.screen);
    let (bw, bh) = (MARKER_BOX[0] * screen[0], MARKER_BOX[1] * screen[1]);
    crate::extras::markers::latest(PAIRS.presents())
        .into_iter()
        .map(|m| Marker { rect: [origin[0] + m.screen[0] - bw * 0.5, origin[1] + m.screen[1] - bh * 0.5, bw, bh], shift: disparity_at(desc, frustum, ipd, m.distance) })
        .collect()
}

/// How the HUD `layer` (kept out of the frame) is laid over a rendered eye's frame of `desc`:
/// each eye where `HudLayer for Dl2` puts it (at HUD distance with its disparity; the whole frame
/// where it says nothing), the crosshair at the aim point, the pieces left out; the left eye
/// (`left`) cuts the pieces out onto the hands' panels first.
pub fn hud_params(queue: &ID3D12CommandQueue, desc: &D3D12_RESOURCE_DESC, layer: &ID3D12Resource, left: bool) -> Params<'static> {
    let size = (desc.Width as u32, desc.Height);
    let pair_head = local(|l| l.pair_head.get());
    let mut params = Params::depth_stereo(0.0, 0, eye_hud_rects(size), aim(size), monaka_stereo::hudover12::CROSS_RADIUS * desc.Height as f32);
    params.ui_out_of_frame = true;
    let mut pieces = LAST_PIECES.locked();
    if left {
        *pieces = [[0.0; 4]; 5];
        for (slot, rect) in pieces.iter_mut().zip(hud_panels::d3d12::publish(queue, layer)) {
            *slot = rect;
        }
    }
    params.pieces = *pieces;
    params.markers = world_markers(desc, Frustum::from_fov(pair_head.fov[0]), pair_head.ipd);
    params
}

/// The pieces of the hands' panels, cut out of a centre frame's layer (depth stereo) for both eyes.
pub fn publish_pieces(queue: &ID3D12CommandQueue, layer: &ID3D12Resource) -> Vec<[f32; 4]> {
    hud_panels::d3d12::publish(queue, layer)
}

/// Where the HUD layer goes over each rendered eye of `size`: where `HudLayer for Dl2` puts it, the
/// whole frame where it says nothing.
fn eye_hud_rects(size: (u32, u32)) -> [[f32; 4]; 2] {
    let whole = monaka_stereo::providers::whole_frame(size);
    std::array::from_fn(|eye| Dl2::over(size, Some(eye)).unwrap_or(whole).rect)
}

/// The HUD `layer` over the features' own images of the eye's frame (`desc`), as over the frame
/// itself (`params`): each eye where `HudLayer for Dl2` puts it, the pieces on the hands left out.
fn lay_beside(queue: &ID3D12CommandQueue, layer: &ID3D12Resource, desc: &D3D12_RESOURCE_DESC, params: &Params) {
    let pieces: Vec<[f32; 4]> = params.pieces.iter().copied().filter(|r| r[2] > 0.0 && r[3] > 0.0).collect();
    monaka_stereo::providers::lay_hud_beside::<Dl2>(queue, layer, (desc.Width as u32, desc.Height), &pieces);
}

/// The rects of the pieces last cut out (the left eye cuts them; the right eye leaves out the same).
static LAST_PIECES: Mutex<[[f32; 4]; 5]> = Mutex::new([[0.0; 4]; 5]);
