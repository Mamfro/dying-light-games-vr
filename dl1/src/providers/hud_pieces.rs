//! The HUD pieces provider for Monaka's dynamic HUD (`monaka_stereo::hud_panels`, its D3D11
//! panels): which of the HUD's draws are which piece's, and where each piece lies in the HUD's
//! layout. The pieces ([`PIECES`]): the minimap on the back of the right hand with the quests
//! beside it, the weapon widget (the held weapon, its ammo or durability) above its palm; health on
//! the back of the left hand, the quick item (the tool slot: grappling hook, throwables, medkits)
//! above its palm.
//!
//! The HUD is every back-buffer draw after the scene copy ([`crate::providers::hud_draws`]); a
//! piece's draw goes into its panel, any other as the game made it. Nothing is cut out of the eye
//! image, so whatever the game lays over the pieces' places (the item wheel) stays whole.
//!
//! Which piece a draw belongs to is the game's own answer, never a match: each piece is one of the
//! game's HUD components (the radar, the two weapon indicators, the quest objectives, health), read
//! once a frame with every element under it ([`crate::providers::hud_widgets`]); the renderer says
//! which element it is drawing as it draws ([`crate::providers::hud_drawing`]). A draw of an
//! element under a panel's component goes into that panel; any other draw (no element, or one of
//! another widget) stays in the eye.
//!
//! A piece shows while its component has anything drawn, at the box of its leaves (a text's taking
//! in its characters, wherever they are drawn).
//!
//! Its probes (`probe_hud`, `hud_hide`) are in `research::hud`.

use crate::Dl1;
use crate::providers::{hud_drawing, hud_widgets};
use monaka_core::hud::LAYOUT_FRESH_MS;
use monaka_producer::game::{Lack, Start};
use monaka_producer::log;
use monaka_stereo::hud_panels::{self, Piece};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::*};
use windows::Win32::Graphics::Direct3D11::*;
use windows::core::Interface;

/// The pieces, in [`hud_widgets::COMPONENTS`]' order: square like the minimap (about twice its own
/// pixels on a 2644x2644 frame); the widget about twice as wide as tall; the quests wide; the
/// others about their pieces' shapes.
pub const PIECES: [Piece; 5] = [
    Piece { name: "minimap", size: (768, 768) },
    Piece { name: "weapon", size: (768, 384) },
    Piece { name: "quests", size: (1152, 480) },
    Piece { name: "health", size: (768, 256) },
    Piece { name: "tool", size: (384, 384) },
];

impl hud_panels::HudPieces for Dl1 {
    const PIECES: &'static [Piece] = &PIECES;
}

/// The install of the dynamic HUD's component (`crate::HUD_PANELS`).
pub(crate) fn install_panels(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    hud_panels::install::<Dl1>(start)?;
    hud_widgets::install(start.hooks, &start.build.engine, &start.build.gamedll)?;
    hud_drawing::install(start.hooks, &start.build.engine)?;
    log!("HUD panels: the minimap and the quests on the back of the right hand, the weapon widget above its palm, health on the back of the left hand, the quick item above its palm (each draw by the UI element the renderer draws)");
    Ok(())
}

/// HUD draws so far in this frame.
static DRAW_INDEX: AtomicU32 = AtomicU32::new(0);
/// Frames since the start; the frames the probe logs and the frame whose vertices it reads.
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// The game's own HUD viewport (the layout the HUD is drawn over).
static BASE: Mutex<D3D11_VIEWPORT> = Mutex::new(D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: 0.0, Height: 0.0, MinDepth: 0.0, MaxDepth: 0.0 });
/// This frame's first HUD draw has run (the pieces shown or hidden for the frame).
static STARTED: AtomicBool = AtomicBool::new(false);

pub fn active() -> bool {
    hud_panels::active()
}

/// A HUD draw is about to run with the layout viewport `base` (the game's own).
pub fn note_viewports(base: D3D11_VIEWPORT) {
    if let Ok(mut b) = BASE.lock() {
        *b = base;
    }
}

/// A HUD draw on the immediate context: true when it is to stay undrawn (a probe's).
pub fn on_hud_draw(context: &ID3D11DeviceContext, vertices: u32) -> bool {
    let index = DRAW_INDEX.fetch_add(1, Relaxed);
    crate::research::hud::draw(context, FRAMES.load(Relaxed), index, vertices)
}

/// Runs a HUD draw (`draw`, into whatever is bound) into the panel of the piece it belongs to, or
/// else `otherwise`: into the eye image as the game made it, or into the HUD layer.
pub fn draw_hud(context: *mut core::ffi::c_void, draw: &dyn Fn(), otherwise: &dyn Fn()) {
    ROUTING[0].fetch_add(1, Relaxed);
    // SAFETY: the game passes its live immediate context.
    let Some(ctx) = (unsafe { ID3D11DeviceContext::from_raw_borrowed(&context) }) else { return otherwise() };
    let Ok(base) = BASE.lock().map(|b| *b) else { return otherwise() };
    let layout = hud_widgets::layout();
    // The space the widgets' boxes are in: the UI root's box, else the viewport the HUD is drawn with.
    let root = layout.as_deref().and_then(|l| l.root).unwrap_or([0.0, 0.0, base.Width, base.Height]);
    let origin = [root[0], root[1]];
    let layout_size = (root[2] - root[0], root[3] - root[1]);
    crate::research::hud::panel_draw(ctx, FRAMES.load(Relaxed), DRAW_INDEX.load(Relaxed).saturating_sub(1));
    // The frame's first HUD draw: the pieces as the game's UI has them now.
    if !STARTED.swap(true, Relaxed)
        && let Some(layout) = layout.as_deref().filter(|l| monaka_channel::tick().saturating_sub(l.taken) < LAYOUT_FRESH_MS)
    {
        if layout.pieces.iter().any(Option::is_some) {
            monaka_producer::log_first!(
                1,
                "HUD panels: the HUD drawn with viewport {:.0},{:.0} {:.0}x{:.0}, laid out in {:?}; the widgets' boxes {:?}",
                base.TopLeftX,
                base.TopLeftY,
                base.Width,
                base.Height,
                root,
                layout.pieces
            );
        }
        for (piece, widget) in layout.pieces.iter().enumerate() {
            match widget {
                Some(b) => hud_panels::show(piece, relative(*b, origin), layout_size),
                None => hud_panels::hide(piece),
            }
        }
    }
    // Whose draw this is: the element the renderer is drawing, if it is under a panel's component.
    let element = hud_drawing::drawing();
    if element != 0 {
        ROUTING[1].fetch_add(1, Relaxed);
    }
    let piece = layout.as_deref().and_then(|l| l.elements.get(&element).copied()).filter(|_| element != 0);
    let Some(piece) = piece else { return otherwise() };
    ROUTING[2].fetch_add(1, Relaxed);
    if hud_panels::d3d11::draw(context, piece, base, draw) {
        ROUTING[3].fetch_add(1, Relaxed);
    } else {
        otherwise();
    }
}

/// A box in layout pixels from the layout's own corner `origin` (left, top, right, bottom).
fn relative(b: [f32; 4], origin: [f32; 2]) -> [f32; 4] {
    [b[0] - origin[0], b[1] - origin[1], b[2] - origin[0], b[3] - origin[1]]
}

/// A present: the panels drawn this frame published; the next frame's HUD draws count from 0.
pub fn frame_end(context: Option<&ID3D11DeviceContext>) {
    DRAW_INDEX.store(0, Relaxed);
    let frame = FRAMES.fetch_add(1, Relaxed);
    STARTED.store(false, Relaxed);
    let Some(context) = context.filter(|_| active()) else { return };
    hud_panels::d3d11::frame_end(context);
    hud_panels::d3d11::each_panel(|panel| crate::research::hud::panel(context, panel.device(), panel.out(), frame, panel.piece()));
}

/// HUD draws routed this run: [seen, drawn by a UI element, by an element of a panel's component,
/// into a panel].
static ROUTING: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// End of a run: how the HUD draws were routed.
pub fn report() {
    if active() || ROUTING[0].load(Relaxed) > 0 {
        let [seen, element, panel_widget, panel] = ROUTING.each_ref().map(|c| c.load(Relaxed));
        log!("HUD panels: {seen} HUD draws routed, {element} drawn by a UI element, {panel_widget} of them under a panel's component, {panel} drawn into a panel");
    }
}
