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
//! Which piece a draw belongs to is the game's own answer: the HUD is the engine's UI tree, each
//! piece one widget of it, read once a frame with its drawable leaves in tree order
//! ([`crate::providers::hud_widgets`]). A draw is matched to the leaf it draws:
//! - a quad by its corner, which the matrix the game writes into the vertex shader's first
//!   constant buffer puts exactly on its leaf's global position (caught as it is written, [`map`],
//!   [`unmap`]);
//! - text, laid out in layout pixels, by its first glyph (read from the vertex buffer the frame's
//!   UI is written into), on its text leaf's row;
//! - a draw matching no leaf (the minimap's map itself, drawn under the radar's background) goes
//!   with the draw before it, unless that one was its widget's last leaf.
//!
//! A piece shows while its widget has anything drawn, at the box of its leaves (a text's taking in
//! its characters, wherever they are drawn).
//!
//! Its probes (`probe_hud`, `hud_hide`) are in `research::hud`.

use crate::Dl1;
use crate::providers::hud_widgets::{self, Leaf};
use monaka_core::hud::LAYOUT_FRESH_MS;
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::game::{Lack, Start};
use monaka_producer::log;
use monaka_stereo::hud_panels::{self, Piece};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
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

/// `ID3D11DeviceContext::Map` and `Unmap` in its vtable.
const CONTEXT_MAP: usize = 14;
const CONTEXT_UNMAP: usize = 15;
/// A text draw's vertices (position first, in layout pixels), as the UI writes them.
const TEXT_STRIDE: u32 = 40;

/// The install of the dynamic HUD's component (`crate::HUD_PANELS`).
pub(crate) fn install_panels(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    hud_panels::install::<Dl1>(start)?;
    let (_, _, context, _) = crate::swapchain_parts(&start.build.renderer)?;
    let context = context.as_raw();
    // SAFETY: method slots of the game's live immediate context; each detour has its signature.
    unsafe {
        start.hooks.method_noted(&MAP, "Map", context, CONTEXT_MAP, map as MapFn, |why| log!("{why}; hooking its vtable slot instead"))?;
        start.hooks.method_noted(&UNMAP, "Unmap", context, CONTEXT_UNMAP, unmap as UnmapFn, |why| log!("{why}; hooking its vtable slot instead"))?;
    }
    hud_widgets::install(start.hooks, &start.build.engine, &start.build.gamedll)?;
    log!("HUD panels: the minimap and the quests on the back of the right hand, the weapon widget above its palm, health on the back of the left hand, the quick item above its palm (each by its widget in the game's UI)");
    Ok(())
}

/// HUD draws so far in this frame.
static DRAW_INDEX: AtomicU32 = AtomicU32::new(0);
/// Frames since the start; the frames the probe logs and the frame whose vertices it reads.
static FRAMES: AtomicU64 = AtomicU64::new(0);
/// The vertex buffer the HUD's text is drawn from, and where the game last wrote it.
static UI_VERTICES: AtomicUsize = AtomicUsize::new(0);
static UI_VERTICES_DATA: AtomicUsize = AtomicUsize::new(0);

/// The game's own HUD viewport (the layout the HUD is drawn over).
static BASE: Mutex<D3D11_VIEWPORT> = Mutex::new(D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: 0.0, Height: 0.0, MinDepth: 0.0, MaxDepth: 0.0 });

/// Where this frame's HUD draws have got to: whether its first has run (the pieces shown or hidden
/// for the frame), the piece the last draw went to, the leaf after the last one matched (draws
/// follow the leaves' order), and whether the last one matched was not its widget's last (a draw
/// matching none goes with it).
struct Routing {
    started: bool,
    owner: Option<usize>,
    cursor: usize,
    open: bool,
}

static ROUTE: Mutex<Routing> = Mutex::new(Routing { started: false, owner: None, cursor: 0, open: false });

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
    let Ok(mut route) = ROUTE.lock() else { return otherwise() };
    let layout = hud_widgets::layout();
    // The space the widgets' boxes are in: the UI root's box, else the viewport the HUD is drawn with.
    let root = layout.as_deref().and_then(|l| l.root).unwrap_or([0.0, 0.0, base.Width, base.Height]);
    let origin = [root[0], root[1]];
    let layout_size = (root[2] - root[0], root[3] - root[1]);
    crate::research::hud::panel_draw(ctx, FRAMES.load(Relaxed), DRAW_INDEX.load(Relaxed).saturating_sub(1));
    // The frame's first HUD draw: the pieces as the game's UI has them now.
    if !route.started {
        *route = Routing { started: true, owner: None, cursor: 0, open: false };
        if let Some(layout) = layout.as_deref().filter(|l| monaka_channel::tick().saturating_sub(l.taken) < LAYOUT_FRESH_MS) {
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
    }
    // Whose draw this is: the leaf it draws, by its quad's corner or its text's first glyph; else
    // the draw before's, unless that was its widget's last leaf (the next widget's draws start).
    let placed = placement(ctx);
    ROUTING[match placed {
        Placement::At(_) => 1,
        Placement::Pixels => 2,
        Placement::Unknown => 3,
    }]
    .fetch_add(1, Relaxed);
    let matched = layout.as_deref().and_then(|layout| match placed {
        Placement::At([x, y]) => hud_widgets::image_leaf(&layout.leaves, route.cursor, [origin[0] + x * layout_size.0, origin[1] + y * layout_size.1]).map(|i| {
            route.cursor = i + 1;
            layout.leaves[i]
        }),
        Placement::Pixels => first_glyph(ctx).inspect(|&glyph| note_text(&layout.leaves, glyph)).and_then(|glyph| text_owner(layout, glyph)),
        Placement::Unknown => None,
    });
    if matched.is_some() {
        ROUTING[4].fetch_add(1, Relaxed);
    }
    match matched {
        Some(leaf) => {
            route.owner = leaf.piece;
            route.open = !leaf.last;
        }
        None if !route.open => route.owner = None,
        None => {}
    }
    let owner = route.owner;
    drop(route);
    let Some(piece) = owner else { return otherwise() };
    if hud_panels::d3d11::draw(context, piece, base, draw) {
        ROUTING[5].fetch_add(1, Relaxed);
    } else {
        otherwise();
    }
}

/// The leaf a text draw is ([`hud_widgets::text_leaf`]); else, for a text spilling out of its own
/// box, the panel widget whose box holds its first glyph. Not the minimap's, whose box is its frame.
fn text_owner(layout: &hud_widgets::Layout, glyph: [f32; 2]) -> Option<Leaf> {
    if let Some(i) = hud_widgets::text_leaf(&layout.leaves, glyph) {
        return Some(layout.leaves[i]);
    }
    let inside = |b: &[f32; 4]| glyph[0] >= b[0] && glyph[0] <= b[2] && glyph[1] >= b[1] && glyph[1] <= b[3];
    let piece = layout.pieces.iter().enumerate().filter(|&(i, _)| i != hud_widgets::MINIMAP).find(|(_, b)| b.as_ref().is_some_and(inside)).map(|(i, _)| i)?;
    let point = [glyph[0], glyph[1], glyph[0], glyph[1]];
    Some(Leaf { piece: Some(piece), text: true, at: glyph, rect: point, drawn: point, last: false })
}

/// Text draws whose first glyph was read, and of them those matched to a text leaf.
static TEXTS: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// A text draw's first glyph against the text leaves; the first unmatched ones (left to their
/// widget's box, [`text_owner`]) are logged with the text leaf nearest by row.
fn note_text(leaves: &[Leaf], glyph: [f32; 2]) {
    TEXTS[0].fetch_add(1, Relaxed);
    if hud_widgets::text_leaf(leaves, glyph).is_some() {
        TEXTS[1].fetch_add(1, Relaxed);
        return;
    }
    static LOGGED: AtomicU64 = AtomicU64::new(0);
    if LOGGED.load(Relaxed) >= 6 {
        return;
    }
    let nearest = leaves.iter().filter(|l| l.text).min_by(|a, b| (a.rect[1] - glyph[1]).abs().total_cmp(&(b.rect[1] - glyph[1]).abs()));
    if let Some(leaf) = nearest {
        LOGGED.fetch_add(1, Relaxed);
        log!("HUD text: a glyph at {:.1},{:.1} outside every text box (its widget's box decides); nearest by row: {:?} (piece {:?})", glyph[0], glyph[1], leaf.rect, leaf.piece);
    }
}

/// A box in layout pixels from the layout's own corner `origin` (left, top, right, bottom).
fn relative(b: [f32; 4], origin: [f32; 2]) -> [f32; 4] {
    [b[0] - origin[0], b[1] - origin[1], b[2] - origin[0], b[3] - origin[1]]
}

/// Where the bound text draw's first glyph is (layout pixels): its first vertex's position, read
/// from where the game last wrote the vertex buffer it draws from (the frame's UI vertices are
/// written before its draws run; quads of glyphs from index 0 of a fixed index buffer).
fn first_glyph(ctx: &ID3D11DeviceContext) -> Option<[f32; 2]> {
    let mut buffers = [None];
    let (mut stride, mut offset) = ([0u32], [0u32]);
    // SAFETY: reads the game's bound vertex buffer into locals on its own context and thread.
    unsafe { ctx.IAGetVertexBuffers(0, 1, Some(buffers.as_mut_ptr()), Some(stride.as_mut_ptr()), Some(offset.as_mut_ptr())) };
    let buffer = buffers[0].as_ref()?.as_raw() as usize;
    if stride[0] != TEXT_STRIDE {
        return None;
    }
    if UI_VERTICES.swap(buffer, Relaxed) != buffer {
        // Not known as the UI's yet: where it was written is learned at its next map.
        UI_VERTICES_DATA.store(0, Relaxed);
        return None;
    }
    let data = UI_VERTICES_DATA.load(Relaxed);
    let vertex = crate::providers::hud_draws::draw_args().base.max(0) as usize;
    let at = data.checked_add(offset[0] as usize + vertex * TEXT_STRIDE as usize).filter(|_| data != 0)?;
    mem::read::<[f32; 2]>(at).filter(|g| g.iter().all(|v| v.is_finite()))
}

/// Where a HUD draw is placed, from the matrix the game wrote for it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Placement {
    /// Its quad's corner (local 0, 0) lands here (fractions of the layout, y down).
    At([f32; 2]),
    /// Laid out in screen pixels (text): the matrix only maps pixels to the screen.
    Pixels,
    /// No matrix caught for its buffer, or not one of the HUD's.
    Unknown,
}

/// The bound draw's placement: its vertex shader's first constant buffer, as last written.
fn placement(ctx: &ID3D11DeviceContext) -> Placement {
    bound_matrix(ctx).map_or(Placement::Unknown, |m| placement_of(&m))
}

/// The bound draw's HUD matrix (its vertex shader's first constant buffer, as last written), if
/// one was caught.
fn bound_matrix(ctx: &ID3D11DeviceContext) -> Option<[f32; 16]> {
    let mut buffers = [None];
    // SAFETY: reads the game's bound constant buffer into a local.
    unsafe { ctx.VSGetConstantBuffers(0, Some(&mut buffers)) };
    let key = buffers[0].as_ref()?.as_raw() as usize;
    MATRICES.lock().ok().and_then(|m| m.iter().find(|(b, _)| *b == key).map(|(_, m)| *m))
}

/// The placement a HUD matrix gives (rows: x, y, z and w of clip space from the local position;
/// the HUD's w row is 0, 0, -1, distance, its quads lying at z 0).
fn placement_of(m: &[f32; 16]) -> Placement {
    let w = m[15];
    if !(w.abs() > 1e-3 && m[12].abs() < 1e-4 && m[13].abs() < 1e-4 && (m[14] + 1.0).abs() < 1e-4) {
        return Placement::Unknown;
    }
    let (x, y) = ((m[3] / w + 1.0) * 0.5, (1.0 - m[7] / w) * 0.5);
    if x.abs() < 1e-3 && y.abs() < 1e-3 { Placement::Pixels } else { Placement::At([x, y]) }
}

/// Whether the HUD's matrices are being caught: between the scene copy and the present.
static SNOOPING: AtomicBool = AtomicBool::new(false);
/// Constant buffers mapped for writing now (buffer, where its contents are written).
static MAPPED: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
/// The first 16 floats each constant buffer was last written with, by buffer.
static MATRICES: Mutex<Vec<(usize, [f32; 16])>> = Mutex::new(Vec::new());
/// Resources already looked at: (resource, a constant buffer of 64 bytes or more).
static CONSTANT_BUFFERS: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());

/// The scene copy has been drawn (`true`) or the frame presented: the HUD's matrices are caught
/// between the two.
pub fn hud_phase(on: bool) {
    SNOOPING.store(on && active(), Relaxed);
}

type MapFn = unsafe extern "system" fn(*mut core::ffi::c_void, *mut core::ffi::c_void, u32, D3D11_MAP, u32, *mut D3D11_MAPPED_SUBRESOURCE) -> windows::core::HRESULT;
type UnmapFn = unsafe extern "system" fn(*mut core::ffi::c_void, *mut core::ffi::c_void, u32);
static MAP: Original<MapFn> = Original::new();
static UNMAP: Original<UnmapFn> = Original::new();

/// Whether `resource` is a constant buffer of 64 bytes or more (remembered by address).
fn is_constant_buffer(resource: *mut core::ffi::c_void) -> bool {
    let key = resource as usize;
    let Ok(mut known) = CONSTANT_BUFFERS.lock() else { return false };
    if let Some(&(_, yes)) = known.iter().find(|(r, _)| *r == key) {
        return yes;
    }
    // SAFETY: the game's live resource, passed to its own Map.
    let yes = unsafe { ID3D11Resource::from_raw_borrowed(&resource) }.and_then(|r| r.cast::<ID3D11Buffer>().ok()).is_some_and(|b| {
        let mut desc = D3D11_BUFFER_DESC::default();
        // SAFETY: reads the buffer's description into a local.
        unsafe { b.GetDesc(&mut desc) };
        desc.BindFlags & D3D11_BIND_CONSTANT_BUFFER.0 as u32 != 0 && desc.ByteWidth >= 64
    });
    if known.len() >= 512 {
        known.clear();
    }
    known.push((key, yes));
    yes
}

/// `ID3D11DeviceContext::Map`: a constant buffer mapped for writing while the HUD is drawn is
/// remembered, to read what the game wrote into it at [`unmap`]; and where the UI's vertex buffer
/// is written ([`first_glyph`]).
unsafe extern "system" fn map(
    context: *mut core::ffi::c_void,
    resource: *mut core::ffi::c_void,
    subresource: u32,
    kind: D3D11_MAP,
    flags: u32,
    mapped: *mut D3D11_MAPPED_SUBRESOURCE,
) -> windows::core::HRESULT {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let result = unsafe { MAP.get()(context, resource, subresource, kind, flags, mapped) };
    if result.is_ok() && subresource == 0 && !mapped.is_null() && resource as usize == UI_VERTICES.load(Relaxed) {
        // SAFETY: the game's call just filled `mapped`.
        UI_VERTICES_DATA.store(unsafe { (*mapped).pData } as usize, Relaxed);
    }
    if result.is_ok() && SNOOPING.load(Relaxed) && kind == D3D11_MAP_WRITE_DISCARD && subresource == 0 && !mapped.is_null() && is_constant_buffer(resource) {
        // SAFETY: the game's call just filled `mapped`.
        let data = unsafe { (*mapped).pData } as usize;
        if data != 0
            && let Ok(mut pending) = MAPPED.lock()
        {
            pending.retain(|(r, _)| *r != resource as usize);
            if pending.len() < 64 {
                pending.push((resource as usize, data));
            }
        }
    }
    result
}

/// `ID3D11DeviceContext::Unmap`: keeps the first 16 floats the game wrote into a remembered
/// constant buffer (a HUD draw's matrix), read while it is still mapped.
unsafe extern "system" fn unmap(context: *mut core::ffi::c_void, resource: *mut core::ffi::c_void, subresource: u32) {
    let _flight = InFlight::enter();
    if subresource == 0
        && let Ok(mut pending) = MAPPED.lock()
        && let Some(at) = pending.iter().position(|(r, _)| *r == resource as usize)
    {
        let (_, data) = pending.swap_remove(at);
        let mut matrix = [0f32; 16];
        // SAFETY: the buffer is still mapped (64 bytes or more, checked at its Map) and its
        // pointer came from the game's own Map.
        unsafe { std::ptr::copy_nonoverlapping(data as *const f32, matrix.as_mut_ptr(), 16) };
        if let Ok(mut matrices) = MATRICES.lock() {
            match matrices.iter().position(|(b, _)| *b == resource as usize) {
                Some(at) => matrices[at].1 = matrix,
                None if matrices.len() < 64 => matrices.push((resource as usize, matrix)),
                None => {}
            }
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { UNMAP.get()(context, resource, subresource) }
}

/// A present: the panels drawn this frame published; the next frame's HUD draws count from 0.
pub fn frame_end(context: Option<&ID3D11DeviceContext>) {
    DRAW_INDEX.store(0, Relaxed);
    let frame = FRAMES.fetch_add(1, Relaxed);
    if let Ok(mut route) = ROUTE.lock() {
        route.started = false;
    }
    let Some(context) = context.filter(|_| active()) else { return };
    hud_panels::d3d11::frame_end(context);
    hud_panels::d3d11::each_panel(|panel| crate::research::hud::panel(context, panel.device(), panel.out(), frame, panel.piece()));
}

/// HUD draws routed this run: [seen, placed by matrix, text in pixels, unplaced, matched to a
/// leaf, into a panel].
static ROUTING: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

/// End of a run: how the HUD draws were routed.
pub fn report() {
    if active() || ROUTING[0].load(Relaxed) > 0 {
        let [seen, at, pixels, unknown, matched, panel] = ROUTING.each_ref().map(|c| c.load(Relaxed));
        log!("HUD panels: {seen} HUD draws routed ({at} placed by their matrix, {pixels} text, {unknown} unplaced), {matched} matched to a widget's part, {panel} drawn into a panel");
        log!("HUD panels: {} text draws read, {} matched to a text leaf", TEXTS[0].load(Relaxed), TEXTS[1].load(Relaxed));
    }
}

#[cfg(test)]
mod tests;
