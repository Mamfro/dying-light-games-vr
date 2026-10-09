//! The in-world HUD of Dying Light 1 (`world_hud=1`): HUD pieces out of the HUD, onto the hands. The minimap goes on the back of the
//! right hand with the quests beside it, the weapon widget (the held weapon, its ammo or
//! durability) above its palm; health on the back of the left hand, the quick item (the tool
//! slot: grappling hook, throwables, medkits) above its palm.
//!
//! The HUD is every back-buffer draw after the scene copy ([`crate::hud::draws`]). Each HUD draw goes
//! either into the eye image, as the game made it, or into one piece's panel, never both: a
//! panel shows its rectangle of the game's HUD layout, mapped to fill it, drawn once onto black
//! and once onto white (the two give its coverage: difference matting, as the HUD layer of
//! `monaka_warp` does). Nothing is cut out of the eye image, so whatever the game lays over the
//! pieces' places (the item wheel) stays whole. Each panel is resolved to premultiplied RGBA and
//! published on its own keyed texture channel, `<channel>-panel-<piece>`, for the viewer to place.
//!
//! Which piece a draw belongs to is the game's own answer: the HUD is the engine's UI tree, each
//! piece one named widget of it (`HudRadar`, `HudPrimaryWeaponIndicator`, `StackObjectives`,
//! `health_wrap`, `HudSecondaryWeaponIndicator`), read once a frame with its drawable leaves
//! ([`crate::hud::ui`]). A draw is matched to the leaf it draws:
//! - a quad by its corner, which the matrix the game writes into the vertex shader's first
//!   constant buffer puts exactly on its leaf's global position (caught as it is written,
//!   [`map`], [`unmap`]);
//! - text, laid out in layout pixels, by its first glyph (read from the vertex buffer the frame's
//!   UI is written into), on its text leaf's row;
//! - a draw matching no leaf (the minimap's map itself, drawn under the radar's background)
//!   goes with the draw before it, unless that one was its widget's last leaf.
//!
//! A piece is shown while its widget has anything drawn, and its panel shows the widget's box
//! (its leaves' union, grown while it stays). Nothing is measured from the image, no piece is
//! found by where it lies (a geometric guess took the experience banner for the health and lost
//! the quests between measurements, 2026-10-07).
//!
//! Its probes (`probe_hud`, `hud_hide`) are in `research::hud`.

use crate::hud::draws::TARGETS;
use crate::hud::ui;
use monaka_hook::{InFlight, Original, mem};
use monaka_channel::ChannelName;
use monaka_core::hud::LAYOUT_FRESH_MS;
use monaka_channel::d3d;
use monaka_channel::keyed::LazyWriter;
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Interface, s};

/// A piece's box grown by this share of its larger side on each side for its panel.
const WIDGET_MARGIN: f32 = 0.06;
/// A text draw's vertices (position first, in layout pixels), as the UI writes them.
const TEXT_STRIDE: u32 = 40;

/// `ID3D11DeviceContext::Map` and `Unmap` in its vtable.
pub const CONTEXT_MAP: usize = 14;
pub const CONTEXT_UNMAP: usize = 15;

/// HUD draws so far in this frame.
static DRAW_INDEX: AtomicU32 = AtomicU32::new(0);
/// Frames since the start; the frames the probe logs and the frame whose vertices it reads.
static FRAMES: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The vertex buffer the HUD's text is drawn from, and where the game last wrote it.
static UI_VERTICES: AtomicUsize = AtomicUsize::new(0);
static UI_VERTICES_DATA: AtomicUsize = AtomicUsize::new(0);

/// A piece of the HUD on its own panel.
struct Piece {
    name: &'static str,
    /// The panel's size (pixels); the piece's rectangle is made this shape.
    size: (u32, u32),
    /// Its widget's box (left, top, right, bottom as fractions of the layout), grown while shown.
    found: Option<[f32; 4]>,
    /// Its rectangle of the HUD layout (x, y, width, height as fractions of it).
    rect: Option<[f32; 4]>,
    /// Shown (its widget drawn): its draws go to its panel, not the eye.
    shown: bool,
    /// Its panel was drawn into this frame; it was published shown the frame before.
    drawn: bool,
    published_shown: bool,
    panel: Option<PanelGpu>,
    failures: u64,
}

impl Piece {
    fn new(name: &'static str, size: (u32, u32)) -> Self {
        Self { name, size, found: None, rect: None, shown: false, drawn: false, published_shown: false, panel: None, failures: 0 }
    }

    /// Grows its box to hold `b` (fractions of the layout) and fits its rectangle to it.
    fn grow(&mut self, b: [f32; 4], layout: (f32, f32)) {
        let found = self.found.map_or(b, |f| ui::union(f, b));
        if self.found != Some(found) {
            self.found = Some(found);
            self.rect = Some(fitted_rect(found, WIDGET_MARGIN, self.size, layout));
        }
    }
}

/// A piece's panel: onto black and onto white (texture, target view, shader view as stored), the
/// resolved image and its channel.
struct PanelGpu {
    targets: [(ID3D11Texture2D, ID3D11RenderTargetView, ID3D11ShaderResourceView); 2],
    out: ID3D11Texture2D,
    out_uav: ID3D11UnorderedAccessView,
    writer: LazyWriter,
}

/// What the pieces share, made on the game's device at the first HUD draw.
struct Gpu {
    device: ID3D11Device,
    format: DXGI_FORMAT,
    resolve: ID3D11ComputeShader,
    constants: ID3D11Buffer,
}

struct Panels {
    channel: ChannelName,
    /// In [`ui::WIDGETS`]' order.
    pieces: [Piece; 5],
    /// The layout: the game's own HUD viewport.
    base: D3D11_VIEWPORT,
    /// The layout size the rectangles are of.
    measured_for: (f32, f32),
    /// Whether this frame's first HUD draw has run (the panels cleared), the piece the last HUD
    /// draw went to, and the leaf after the last one matched (draws follow the leaves' order).
    started: bool,
    owner: Option<usize>,
    cursor: usize,
    /// The last draw matched was not its widget's last leaf: a draw matching none goes with it.
    open: bool,
    gpu: Option<Gpu>,
    failures: u64,
}

static PANELS: Mutex<Option<Panels>> = Mutex::new(None);

const RESOLVE: &str = r"
cbuffer Size : register(b0) { uint W, H, pad0, pad1; };
Texture2D<float4> Black : register(t0);
Texture2D<float4> White : register(t1);
RWTexture2D<unorm float4> Out : register(u0);
[numthreads(8, 8, 1)] void Resolve(uint3 id : SV_DispatchThreadID) {
    if (id.x >= W || id.y >= H) return;
    float3 black = Black.Load(int3(id.xy, 0)).rgb, white = White.Load(int3(id.xy, 0)).rgb;
    float a = saturate(1 - dot(white - black, float3(1, 1, 1) / 3));
    Out[id.xy] = float4(saturate(black), a);
}
";

/// Turns the panels on; each piece goes to `<channel>-panel-<piece>`.
pub fn install(channel: &ChannelName) {
    *PANELS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Panels {
        channel: channel.clone(),
        // Square like the minimap (about twice its own pixels on a 2644x2644 frame); the widget
        // about twice as wide as tall; the quests wide; the others about their pieces' shapes.
        pieces: [
            Piece::new("minimap", (768, 768)),
            Piece::new("weapon", (768, 384)),
            Piece::new("quests", (1152, 480)),
            Piece::new("health", (768, 256)),
            Piece::new("tool", (384, 384)),
        ],
        base: D3D11_VIEWPORT::default(),
        measured_for: (0.0, 0.0),
        started: false,
        owner: None,
        cursor: 0,
        open: false,
        gpu: None,
        failures: 0,
    });
    ACTIVE.store(true, Release);
    log!("HUD panels: the minimap and the quests on the back of the right hand, the weapon widget above its palm, health on the back of the left hand, the quick item above its palm (each by its widget in the game's UI)");
}

pub fn active() -> bool {
    ACTIVE.load(Relaxed)
}

/// A HUD draw is about to run with the layout viewport `base` (the game's own).
pub fn note_viewports(base: D3D11_VIEWPORT) {
    if let Ok(mut panels) = PANELS.lock()
        && let Some(panels) = panels.as_mut()
    {
        panels.base = base;
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
    let Ok(mut guard) = PANELS.lock() else { return otherwise() };
    let Some(panels) = guard.as_mut() else { return otherwise() };
    if panels.gpu.is_none() {
        match Gpu::new(ctx) {
            Ok(gpu) => panels.gpu = Some(gpu),
            Err(e) => {
                if panels.failures == 0 {
                    log!("HUD panels unavailable: {e}");
                }
                panels.failures += 1;
                drop(guard);
                ACTIVE.store(false, Relaxed);
                return otherwise();
            }
        }
    }
    let base = panels.base;
    let layout = ui::layout();
    // The space the widgets' boxes are in: the UI root's box, else the viewport the HUD is drawn with.
    let root = layout.as_deref().and_then(|l| l.root).unwrap_or([0.0, 0.0, base.Width, base.Height]);
    let origin = [root[0], root[1]];
    let layout_size = (root[2] - root[0], root[3] - root[1]);
    // A new layout size: the boxes are of the old one.
    if panels.measured_for != layout_size {
        panels.measured_for = layout_size;
        for piece in &mut panels.pieces {
            piece.found = None;
            piece.rect = None;
        }
    }
    let Panels { pieces, gpu, started, owner, cursor, open, channel, .. } = panels;
    let gpu = gpu.as_mut().expect("made above");
    crate::research::hud::panel_draw(ctx, &gpu.device, FRAMES.load(Relaxed), DRAW_INDEX.load(Relaxed).saturating_sub(1));
    // The frame's first HUD draw: the pieces as the game's UI has them now; each shown piece's
    // panel starts empty (and goes out each frame, empty when the piece drew nothing).
    if !*started {
        *started = true;
        *owner = None;
        *cursor = 0;
        *open = false;
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
            for (piece, widget) in pieces.iter_mut().zip(&layout.pieces) {
                match widget {
                    Some(b) => piece.grow(fractions_of(*b, origin, layout_size), layout_size),
                    None => piece.found = None,
                }
                set_shown(piece, widget.is_some());
            }
        }
        for piece in pieces.iter_mut().filter(|p| p.shown) {
            if !ensure_panel(gpu, channel, piece) {
                continue;
            }
            let panel = piece.panel.as_ref().expect("made above");
            // SAFETY: clears our own targets on the game's context.
            unsafe {
                ctx.ClearRenderTargetView(&panel.targets[0].1, &[0.0, 0.0, 0.0, 0.0]);
                ctx.ClearRenderTargetView(&panel.targets[1].1, &[1.0, 1.0, 1.0, 1.0]);
            }
            piece.drawn = true;
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
        Placement::At([x, y]) => ui::image_leaf(&layout.leaves, *cursor, [origin[0] + x * layout_size.0, origin[1] + y * layout_size.1]).map(|i| {
            *cursor = i + 1;
            layout.leaves[i]
        }),
        Placement::Pixels => first_glyph(ctx).and_then(|glyph| ui::text_leaf(&layout.leaves, glyph).map(|i| (glyph, layout.leaves[i]))).map(|(glyph, leaf)| {
            // A text longer than its box (right-aligned, it starts left of it): the piece's box
            // takes it in.
            // Not the minimap's: its box is its frame ([`ui::MINIMAP`]).
            if let Some(piece) = leaf.piece.filter(|&i| i != ui::MINIMAP).and_then(|i| pieces.get_mut(i)).filter(|p| p.shown) {
                piece.grow(fractions_of([glyph[0], leaf.rect[1], leaf.rect[2], leaf.rect[3]], origin, layout_size), layout_size);
            }
            leaf
        }),
        Placement::Unknown => None,
    });
    if matched.is_some() {
        ROUTING[4].fetch_add(1, Relaxed);
    }
    match matched {
        Some(leaf) => {
            *owner = leaf.piece;
            *open = !leaf.last;
        }
        None if !*open => *owner = None,
        None => {}
    }
    let target = (*owner).and_then(|i| {
        let piece = &pieces[i];
        Some((piece.panel.as_ref().filter(|_| piece.drawn && piece.shown)?, piece.size, piece.rect?))
    });
    match target {
        Some((panel, size, rect)) => {
            ROUTING[5].fetch_add(1, Relaxed);
            draw_into_panel(ctx, context, panel, size, base, rect, draw)
        }
        None => otherwise(),
    }
}

/// HUD draws routed this run: [seen, placed by matrix, text in pixels, unplaced, matched to a
/// leaf, into a panel].
static ROUTING: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

/// End of a run: how the HUD draws were routed.
pub fn report() {
    if active() || ROUTING[0].load(Relaxed) > 0 {
        let [seen, at, pixels, unknown, matched, panel] = ROUTING.each_ref().map(|c| c.load(Relaxed));
        log!("HUD panels: {seen} HUD draws routed ({at} placed by their matrix, {pixels} text, {unknown} unplaced), {matched} matched to a widget's part, {panel} drawn into a panel");
    }
}

/// A box in layout pixels as fractions of the layout at `origin` (left, top, right, bottom).
fn fractions_of(b: [f32; 4], origin: [f32; 2], (width, height): (f32, f32)) -> [f32; 4] {
    [(b[0] - origin[0]) / width, (b[1] - origin[1]) / height, (b[2] - origin[0]) / width, (b[3] - origin[1]) / height]
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
    let vertex = crate::hud::draws::draw_args().base.max(0) as usize;
    let at = data.checked_add(offset[0] as usize + vertex * TEXT_STRIDE as usize).filter(|_| data != 0)?;
    mem::read::<[f32; 2]>(at).filter(|g| g.iter().all(|v| v.is_finite()))
}

/// Makes `piece`'s panel on the channel `<game channel>-panel-<piece>` if it has none; false when
/// it cannot (the piece is not shown then).
fn ensure_panel(gpu: &Gpu, channel: &ChannelName, piece: &mut Piece) -> bool {
    if piece.panel.is_some() {
        return true;
    }
    let made = channel.panel(piece.name).ok_or_else(|| "channel name too long".to_string()).and_then(|name| gpu.panel(&name, piece.size));
    match made {
        Ok(panel) => {
            log!("HUD panel {}: {}x{} (format {} like the game's HUD target)", piece.name, piece.size.0, piece.size.1, gpu.format.0);
            piece.panel = Some(panel);
            true
        }
        Err(e) => {
            if piece.failures == 0 {
                log!("HUD panel {} unavailable: {e}", piece.name);
            }
            piece.failures += 1;
            piece.shown = false;
            false
        }
    }
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

pub type MapFn = unsafe extern "system" fn(*mut core::ffi::c_void, *mut core::ffi::c_void, u32, D3D11_MAP, u32, *mut D3D11_MAPPED_SUBRESOURCE) -> windows::core::HRESULT;
pub type UnmapFn = unsafe extern "system" fn(*mut core::ffi::c_void, *mut core::ffi::c_void, u32);
pub static MAP: Original<MapFn> = Original::new();
pub static UNMAP: Original<UnmapFn> = Original::new();

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
pub unsafe extern "system" fn map(
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
pub unsafe extern "system" fn unmap(context: *mut core::ffi::c_void, resource: *mut core::ffi::c_void, subresource: u32) {
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

/// A present: resolves and publishes the panels drawn this frame; the next frame's HUD draws count
/// from 0.
pub fn frame_end(context: Option<&ID3D11DeviceContext>) {
    DRAW_INDEX.store(0, Relaxed);
    FRAMES.fetch_add(1, Relaxed);
    let Some(context) = context.filter(|_| active()) else { return };
    let Ok(mut guard) = PANELS.lock() else { return };
    let Some(panels) = guard.as_mut() else { return };
    panels.started = false;
    let Some(gpu) = panels.gpu.as_mut() else { return };
    // The panels: drawn ones resolved and published; one that went away published cleared, once.
    for piece in &mut panels.pieces {
        let drawn = std::mem::take(&mut piece.drawn);
        let went = piece.published_shown && !piece.shown;
        if !drawn && !went {
            continue;
        }
        let Some(panel) = &mut piece.panel else { continue };
        if !drawn {
            // SAFETY: clears our own targets on the game's context.
            unsafe {
                context.ClearRenderTargetView(&panel.targets[0].1, &[0.0, 0.0, 0.0, 0.0]);
                context.ClearRenderTargetView(&panel.targets[1].1, &[1.0, 1.0, 1.0, 1.0]);
            }
        }
        piece.published_shown = drawn;
        resolve(context, gpu, panel, piece.size);
        let Some(writer) = panel.writer.get(&gpu.device, piece.size.0, piece.size.1).ready(|note| log!("HUD panel {}: {note}", piece.name)) else { continue };
        if let Err(e) = writer.publish(context, [&panel.out, &panel.out], monaka_channel::tick()) {
            if piece.failures == 0 {
                log!("HUD panel {} publish failed: {e}", piece.name);
            }
            piece.failures += 1;
        }
        crate::research::hud::panel(context, &gpu.device, &panel.out, FRAMES.load(Relaxed) - 1, piece.name);
    }
}

/// Shows a piece while its widget is drawn (and it has a rectangle).
fn set_shown(piece: &mut Piece, drawn: bool) {
    let shown = drawn && piece.rect.is_some();
    if shown != piece.shown {
        log!("HUD panel {}: {}", piece.name, if shown { "drawn: on the hand" } else { "gone (a menu, or nothing to show): the HUD is left whole there" });
    }
    piece.shown = shown;
}

/// Runs a HUD draw into a piece's panel, onto black and onto white, with the layout scaled and
/// moved so `rect` fills it; the game's bindings are given back.
fn draw_into_panel(ctx: &ID3D11DeviceContext, context: *mut core::ffi::c_void, panel: &PanelGpu, size: (u32, u32), base: D3D11_VIEWPORT, rect: [f32; 4], draw: &dyn Fn()) {
    let mut bound = [None];
    let mut depth = None;
    let (mut viewport_count, mut scissor_count) = (1u32, 1u32);
    let mut viewport = [D3D11_VIEWPORT::default()];
    let mut scissors = [RECT::default()];
    // SAFETY: reads the game's bound state into locals on its own context and thread.
    let raster: Option<ID3D11RasterizerState> = unsafe { ctx.RSGetState() }.ok();
    // SAFETY: as above.
    unsafe {
        ctx.OMGetRenderTargets(Some(&mut bound), Some(&mut depth));
        ctx.RSGetViewports(&mut viewport_count, Some(viewport.as_mut_ptr()));
        ctx.RSGetScissorRects(&mut scissor_count, Some(scissors.as_mut_ptr()));
    }
    let game_scissors = scissor_count > 0
        && raster.as_ref().is_some_and(|r| {
            let mut desc = D3D11_RASTERIZER_DESC::default();
            // SAFETY: reads the state's description into a local.
            unsafe { r.GetDesc(&mut desc) };
            desc.ScissorEnable.as_bool()
        });
    let (width, height) = (size.0 as f32, size.1 as f32);
    let (sx, sy) = (width / (rect[2] * base.Width), height / (rect[3] * base.Height));
    let panel_view = D3D11_VIEWPORT {
        TopLeftX: -rect[0] * base.Width * sx,
        TopLeftY: -rect[1] * base.Height * sy,
        Width: base.Width * sx,
        Height: base.Height * sy,
        MinDepth: base.MinDepth,
        MaxDepth: base.MaxDepth,
    };
    // The game's own scissor (text boxes are clipped by one), set in the back buffer's pixels of
    // its own layout (placing the HUD moves only the viewport), carried into the panel.
    let full = RECT { left: 0, top: 0, right: size.0 as i32, bottom: size.1 as i32 };
    let clip = if game_scissors {
        let x = |v: i32| ((v as f32 - base.TopLeftX) * sx + panel_view.TopLeftX).floor() as i32;
        let y = |v: i32| ((v as f32 - base.TopLeftY) * sy + panel_view.TopLeftY).floor() as i32;
        let s = scissors[0];
        intersect(&full, &RECT { left: x(s.left), top: y(s.top), right: x(s.right), bottom: y(s.bottom) })
    } else {
        full
    };
    // SAFETY: binds our targets through the original OMSetRenderTargets (our hook stays out of
    // it), draws, and gives back the game's targets, viewport and scissor.
    unsafe {
        if clip.right > clip.left && clip.bottom > clip.top {
            ctx.RSSetViewports(Some(&[panel_view]));
            ctx.RSSetScissorRects(Some(&[clip]));
            for target in &panel.targets {
                let view = [target.1.as_raw()];
                TARGETS.get()(context, 1, view.as_ptr(), std::ptr::null_mut());
                draw();
            }
        }
        let game = [bound[0].as_ref().map_or(std::ptr::null_mut(), |v| v.as_raw())];
        TARGETS.get()(context, 1, game.as_ptr(), depth.as_ref().map_or(std::ptr::null_mut(), |d| d.as_raw()));
        ctx.RSSetViewports(Some(&viewport[..viewport_count.min(1) as usize]));
        ctx.RSSetScissorRects(Some(&scissors[..scissor_count.min(1) as usize]));
    }
}

fn intersect(a: &RECT, b: &RECT) -> RECT {
    RECT { left: a.left.max(b.left), top: a.top.max(b.top), right: a.right.min(b.right), bottom: a.bottom.min(b.bottom) }
}

impl Gpu {
    fn new(context: &ID3D11DeviceContext) -> Result<Self, String> {
        // SAFETY: COM calls on the game's live context and device.
        let device = unsafe { context.GetDevice() }.map_err(|e| e.to_string())?;
        // The game's own target format, so the panels' texels are what the eye image's would be.
        let mut bound = [None];
        // SAFETY: reads the bound render target into a local.
        unsafe { context.OMGetRenderTargets(Some(&mut bound), None) };
        let format = bound[0]
            .as_ref()
            .map(|view| {
                let mut desc = D3D11_RENDER_TARGET_VIEW_DESC::default();
                // SAFETY: reads the view's description into a local.
                unsafe { view.GetDesc(&mut desc) };
                desc.Format
            })
            .unwrap_or(DXGI_FORMAT_R8G8B8A8_UNORM);
        if !matches!(format, DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R8G8B8A8_UNORM_SRGB) {
            return Err(format!("the HUD draws into format {}, not RGBA8", format.0));
        }
        let code = monaka_channel::d3d12::compute::compile(RESOLVE, "panel resolve", s!("Resolve"))?;
        let mut resolve = None;
        // SAFETY: bytecode the compiler just made, and a local out-pointer.
        unsafe { device.CreateComputeShader(monaka_channel::d3d12::compute::blob_bytes(&code), None, Some(&mut resolve)) }.map_err(|e| e.to_string())?;
        let buffer_desc = D3D11_BUFFER_DESC { ByteWidth: 16, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
        let mut constants = None;
        // SAFETY: valid descriptor and out-pointer.
        unsafe { device.CreateBuffer(&buffer_desc, None, Some(&mut constants)) }.map_err(|e| e.to_string())?;
        Ok(Self {
            device,
            format,
            resolve: resolve.expect("shader created"),
            constants: constants.expect("buffer created"),
        })
    }

    /// A render target view of `texture` in the game's HUD format.
    fn target_view(&self, texture: &ID3D11Texture2D) -> Option<ID3D11RenderTargetView> {
        let desc = D3D11_RENDER_TARGET_VIEW_DESC {
            Format: self.format,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 } },
        };
        let mut view = None;
        // SAFETY: a view of our own texture in a format of its family.
        unsafe { self.device.CreateRenderTargetView(texture, Some(&desc), Some(&mut view)) }.ok()?;
        view
    }

    /// A piece's panel of `size` on the channel `<game channel>-panel-<name>`.
    fn panel(&self, channel: &ChannelName, size: (u32, u32)) -> Result<PanelGpu, String> {
        let target = || -> Result<(ID3D11Texture2D, ID3D11RenderTargetView, ID3D11ShaderResourceView), String> {
            let mut desc = d3d::simple_texture_desc(size.0, size.1, DXGI_FORMAT_R8G8B8A8_TYPELESS);
            desc.BindFlags = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
            let texture = d3d::create_texture(&self.device, &desc).map_err(|e| e.to_string())?;
            let rtv = self.target_view(&texture).ok_or("render target view")?;
            let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                ViewDimension: windows::Win32::Graphics::Direct3D::D3D_SRV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 } },
            };
            let mut srv = None;
            // SAFETY: a view of our own texture in a format of its family.
            unsafe { self.device.CreateShaderResourceView(&texture, Some(&srv_desc), Some(&mut srv)) }.map_err(|e| e.to_string())?;
            Ok((texture, rtv, srv.expect("view created")))
        };
        let targets = [target()?, target()?];
        let mut out_desc = d3d::simple_texture_desc(size.0, size.1, DXGI_FORMAT_R8G8B8A8_UNORM);
        out_desc.BindFlags = (D3D11_BIND_UNORDERED_ACCESS.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
        let out = d3d::create_texture(&self.device, &out_desc).map_err(|e| e.to_string())?;
        let mut out_uav = None;
        // SAFETY: a view of our own texture in its own format.
        unsafe { self.device.CreateUnorderedAccessView(&out, None, Some(&mut out_uav)) }.map_err(|e| e.to_string())?;
        Ok(PanelGpu { targets, out, out_uav: out_uav.expect("view created"), writer: LazyWriter::new(channel.clone()) })
    }

}

/// A box (left, top, right, bottom as fractions of the layout) grown by `margin` of its larger
/// side on each side and widened or heightened about its centre to the panel's shape, as a
/// rectangle (x, y, width, height as fractions of the layout `layout` pixels).
fn fitted_rect(b: [f32; 4], margin: f32, panel: (u32, u32), (layout_w, layout_h): (f32, f32)) -> [f32; 4] {
    let rect = monaka_core::hud::fit([b[0] * layout_w, b[1] * layout_h, b[2] * layout_w, b[3] * layout_h], margin, panel);
    [rect[0] / layout_w, rect[1] / layout_h, rect[2] / layout_w, rect[3] / layout_h]
}

/// A panel's black and white draws into premultiplied RGBA, on the game's context, its compute
/// bindings given back after.
fn resolve(context: &ID3D11DeviceContext, gpu: &Gpu, panel: &PanelGpu, size: (u32, u32)) {
    let constants = [size.0, size.1, 0, 0];
    let mut shader = None;
    let mut views: [Option<ID3D11ShaderResourceView>; 2] = [None, None];
    let mut uav = [None];
    let mut buffers = [None];
    // SAFETY: reads the game's compute bindings, binds ours, dispatches once and gives them back.
    unsafe {
        context.CSGetShader(&mut shader, None, None);
        context.CSGetShaderResources(0, Some(&mut views));
        context.CSGetUnorderedAccessViews(0, Some(&mut uav));
        context.CSGetConstantBuffers(0, Some(&mut buffers));
        context.UpdateSubresource(&gpu.constants, 0, None, constants.as_ptr().cast(), 0, 0);
        context.CSSetConstantBuffers(0, Some(&[Some(gpu.constants.clone())]));
        context.CSSetShaderResources(0, Some(&[Some(panel.targets[0].2.clone()), Some(panel.targets[1].2.clone())]));
        context.CSSetUnorderedAccessViews(0, 1, Some([Some(panel.out_uav.clone())].as_ptr()), None);
        context.CSSetShader(&gpu.resolve, None);
        context.Dispatch(size.0.div_ceil(8), size.1.div_ceil(8), 1);
        context.CSSetUnorderedAccessViews(0, 1, Some(uav.as_ptr()), None);
        context.CSSetShaderResources(0, Some(&views));
        context.CSSetConstantBuffers(0, Some(&buffers));
        context.CSSetShader(shader.as_ref(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piece_box_becomes_its_panel_shape() {
        // Made twice as wide as tall about its centre.
        let rect = fitted_rect([0.846, 0.710, 0.921, 0.749], WIDGET_MARGIN, (768, 384), (2644.0, 2644.0));
        assert!((rect[2] / rect[3] - 2.0).abs() < 1e-4, "{rect:?}");
        assert!((rect[0] + rect[2] / 2.0 - 0.8835).abs() < 1e-4 && (rect[1] + rect[3] / 2.0 - 0.7295).abs() < 1e-4);
        // Grown only while shown.
        let mut piece = Piece::new("weapon", (768, 384));
        piece.grow([0.8, 0.7, 0.9, 0.75], (2644.0, 2644.0));
        piece.grow([0.85, 0.72, 0.95, 0.74], (2644.0, 2644.0));
        assert_eq!(piece.found, Some([0.8, 0.7, 0.95, 0.75]));
    }

    #[test]
    fn hud_matrices_place_quads_and_leave_text_in_pixels() {
        // Logged 2026-10-06: a minimap icon (its corner where the map saw it, 0.885 x 0.245), a
        // rotated minimap element, and quest text (pixels to the screen).
        let icon = [2.414, 0.0, 0.0, 2451.858, 0.0, -2.414, 0.0, 1640.567, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 3191.590];
        let Placement::At([x, y]) = placement_of(&icon) else { panic!("{:?}", placement_of(&icon)) };
        assert!((x - 0.884).abs() < 0.002 && (y - 0.243).abs() < 0.002, "{x} {y}");
        let rotated = [-1.619, 1.791, 0.0, 2194.469, 1.791, 1.619, 0.0, 875.911, 0.0, 0.0, 0.0, 1.0, -0.0, 0.0, -1.0, 3191.590];
        assert!(matches!(placement_of(&rotated), Placement::At(_)));
        let text = [2.414, 0.0, 0.0, -3191.590, 0.0, -2.414, 0.0, 3191.590, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 3191.590];
        assert_eq!(placement_of(&text), Placement::Pixels);
        // Another shader's buffer (its last row is not the HUD's).
        let other = [-1.106, 1.543, 0.0, 0.684, 1.619, -1.791, 0.0, 2517.355, -1.791, -1.619, 0.0, -936.189, 0.0, 0.0, 0.0, 1.0];
        assert_eq!(placement_of(&other), Placement::Unknown);
    }
}
