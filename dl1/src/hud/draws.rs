//! HUD and menus. The frame's structure: the finished 3D image reaches the back buffer as one
//! unblended three-vertex draw, and every later draw to the back buffer in that frame is HUD or
//! menu. Those draws get a viewport at the HUD's place in the eye ([`Placement`]: shrunk toward
//! the eye's straight-ahead point, as each eye's image is off-centre, and shifted toward the
//! nose), so the HUD floats readable in front of the world instead of in the far periphery at
//! infinity.

use crate::view::stereo;
use monaka_hook::{InFlight, Original};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BLEND_DESC, D3D11_DEVICE_CONTEXT_IMMEDIATE, D3D11_VIEWPORT, ID3D11BlendState, ID3D11DeviceContext, ID3D11RenderTargetView,
    ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;
use windows::core::Interface;

use monaka_core::hud::{Placement, Rect};

pub struct HudSettings {
    /// How far away the HUD floats, metres.
    pub distance: f32,
    pub scale: f32,
    pub backbuffer_width: u32,
    pub backbuffer_height: u32,
}

static SETTINGS: Mutex<Option<HudSettings>> = Mutex::new(None);

pub fn install(settings: HudSettings) {
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(settings);
}

/// One frame's progress on the back buffer.
struct Pass {
    backbuffer: usize,
    scene_copied: bool,
    shifted: bool,
    base: D3D11_VIEWPORT,
    last_set_x: f32,
}

static PASS: Mutex<Pass> = Mutex::new(Pass {
    backbuffer: 0,
    scene_copied: false,
    shifted: false,
    base: D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: 0.0, Height: 0.0, MinDepth: 0.0, MaxDepth: 0.0 },
    last_set_x: 0.0,
});
/// Fast path for every draw: is the immediate context drawing to the back buffer?
static ON_BACKBUFFER: AtomicBool = AtomicBool::new(false);
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
    SETTINGS.lock().ok()?.as_ref().map(|s| (s.backbuffer_width, s.backbuffer_height))
}

pub fn scene_copies() -> u64 {
    SCENE_COPIES.load(Relaxed)
}

pub type DrawIndexedFn = unsafe extern "system" fn(*mut core::ffi::c_void, u32, u32, i32);
pub type DrawFn = unsafe extern "system" fn(*mut core::ffi::c_void, u32, u32);
pub type DrawIndexedInstancedFn = unsafe extern "system" fn(*mut core::ffi::c_void, u32, u32, u32, i32, u32);
pub type DrawInstancedFn = unsafe extern "system" fn(*mut core::ffi::c_void, u32, u32, u32, u32);
pub type TargetsFn = unsafe extern "system" fn(*mut core::ffi::c_void, u32, *const *mut core::ffi::c_void, *mut core::ffi::c_void);

pub static DRAW_INDEXED: Original<DrawIndexedFn> = Original::new();
pub static DRAW: Original<DrawFn> = Original::new();
pub static DRAW_INDEXED_INSTANCED: Original<DrawIndexedInstancedFn> = Original::new();
pub static DRAW_INSTANCED: Original<DrawInstancedFn> = Original::new();
pub static TARGETS: Original<TargetsFn> = Original::new();

/// Called at each present: the next frame starts; remember this swapchain's back buffer and
/// leave the game's own viewport behind.
pub fn frame_end(chain: &IDXGISwapChain) {
    if !ENABLED.load(Acquire) {
        return;
    }
    let mut pass = PASS.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: COM calls on the game's live swapchain.
    unsafe {
        if let Ok(back) = chain.GetBuffer::<ID3D11Texture2D>(0) {
            pass.backbuffer = back.as_raw() as usize;
        }
        if pass.shifted
            && let Ok(device) = chain.GetDevice::<windows::Win32::Graphics::Direct3D11::ID3D11Device>()
            && let Ok(context) = device.GetImmediateContext()
        {
            context.RSSetViewports(Some(&[pass.base]));
        }
    }
    pass.scene_copied = false;
    pass.shifted = false;
    crate::output::hybrid::frame_end();
    crate::research::targets::frame_end();
    crate::hud::panels::hud_phase(false);
    // SAFETY: COM calls on the game's live swapchain.
    let context = crate::hud::panels::active().then(|| unsafe { chain.GetDevice::<windows::Win32::Graphics::Direct3D11::ID3D11Device>().and_then(|d| d.GetImmediateContext()) }.ok()).flatten();
    crate::hud::panels::frame_end(context.as_ref());
    ON_BACKBUFFER.store(false, Relaxed);
}

fn immediate(context: &ID3D11DeviceContext) -> bool {
    // SAFETY: COM call on a live context.
    unsafe { context.GetType() == D3D11_DEVICE_CONTEXT_IMMEDIATE }
}

/// `OMSetRenderTargets`.
pub unsafe extern "system" fn set_targets(
    context: *mut core::ffi::c_void,
    count: u32,
    views: *const *mut core::ffi::c_void,
    depth: *mut core::ffi::c_void,
) {
    let _flight = InFlight::enter();
    // SAFETY: the game passes its live context and an array of `count` views.
    unsafe {
        if stereo::capturing()
            && let Some(ctx) = ID3D11DeviceContext::from_raw_borrowed(&context)
            && immediate(ctx)
        {
            let mut on = false;
            if (1..=8).contains(&count) && !views.is_null() {
                let first = *views;
                let backbuffer = PASS.lock().map(|p| p.backbuffer).unwrap_or(0);
                if let Some(view) = ID3D11RenderTargetView::from_raw_borrowed(&first)
                    && backbuffer != 0
                {
                    let resource: Option<ID3D11Resource> = view.GetResource().ok();
                    on = resource.is_some_and(|r| r.as_raw() as usize == backbuffer);
                }
            }
            ON_BACKBUFFER.store(on, Relaxed);
            crate::research::backbuffer::bound(ctx, on, count, !depth.is_null());
            crate::research::targets::bound(count, views, !depth.is_null());
            crate::research::depth::bound(depth);
            if !depth.is_null() && (crate::output::hybrid::enabled() || crate::output::upscale::enabled()) {
                crate::output::hybrid::depth_bound(depth as usize);
            }
            if crate::output::upscale::enabled() {
                crate::output::upscale::bound(count, views, !depth.is_null());
            }
        }
        TARGETS.get()(context, count, views, depth)
    }
}

/// Draw calls the hooks saw: whether they stay hooked while the game runs.
static DRAWS_SEEN: AtomicU64 = AtomicU64::new(0);

pub fn draws_seen() -> u64 {
    DRAWS_SEEN.load(Relaxed)
}

/// Before each draw: tracks the frame on the back buffer and places HUD draws. Returns the HUD
/// layer's targets when this draw belongs in the layer (alternate-eye with depth) instead.
fn before_draw(context: *mut core::ffi::c_void, vertices: u32) -> Option<[ID3D11RenderTargetView; 2]> {
    DRAWS_SEEN.fetch_add(1, Relaxed);
    if crate::research::targets::enabled()
        && stereo::capturing()
        // SAFETY: the game passes its live context.
        && unsafe { ID3D11DeviceContext::from_raw_borrowed(&context) }.is_some_and(immediate)
    {
        crate::research::targets::draw(vertices);
    }
    if !ON_BACKBUFFER.load(Relaxed) || !stereo::capturing() {
        return None;
    }
    // SAFETY: the game passes its live context.
    let context = unsafe { ID3D11DeviceContext::from_raw_borrowed(&context) }?;
    if !immediate(context) {
        return None;
    }
    let mut pass = PASS.lock().unwrap_or_else(|e| e.into_inner());
    crate::research::backbuffer::draw(context, vertices, pass.scene_copied);
    if !pass.scene_copied {
        if vertices == 3 {
            let mut blend: Option<ID3D11BlendState> = None;
            let mut desc = D3D11_BLEND_DESC::default();
            // SAFETY: reads the bound blend state into locals.
            unsafe {
                context.OMGetBlendState(Some(&mut blend), None, None);
                if let Some(blend) = &blend {
                    blend.GetDesc(&mut desc);
                }
            }
            pass.scene_copied = !desc.RenderTarget[0].BlendEnable.as_bool();
            if pass.scene_copied {
                SCENE_COPIES.fetch_add(1, Relaxed);
                crate::research::depth::scene_copied();
                crate::research::targets::scene_copied(context);
            }
            if pass.scene_copied {
                crate::output::hybrid::scene_copied();
                crate::hud::panels::hud_phase(true);
            }
        }
        return None;
    }
    // The first draw after the scene copy: the back buffer holds the world, not yet the HUD.
    if (crate::output::hybrid::needs_capture() || crate::output::upscale::needs_capture())
        && let Some(frame) = stereo::drawing_present()
    {
        let mut target = [None];
        // SAFETY: reads the bound render target into a local.
        unsafe { context.OMGetRenderTargets(Some(&mut target), None) };
        // SAFETY: COM calls on the bound render target view and its resource.
        let image = target[0].as_ref().and_then(|view| unsafe { view.GetResource() }.ok()).and_then(|r| r.cast::<ID3D11Texture2D>().ok());
        if let Some(image) = image {
            crate::output::hybrid::capture(context, &image, frame);
            crate::output::upscale::capture(context, &image, frame);
        }
    }
    if crate::hud::panels::on_hud_draw(context, vertices) {
        SKIP_DRAW.store(true, Relaxed);
    }
    let frame = stereo::drawing_present()?;
    if crate::output::hybrid::hud_layer() || crate::output::upscale::enabled() {
        // Drawn at the game's own layout into the layer; each eye places it when composited (by
        // the hybrid, or over the upscaled eye). The size is copied out: the layer owners take
        // their own locks, and the present holds theirs while asking for the HUD's rectangles.
        let (width, height) = SETTINGS.lock().ok()?.as_ref().map(|s| (s.backbuffer_width, s.backbuffer_height))?;
        drop(pass);
        // The fixed UI (`hud::ui_size`) goes into an FSR layer of its own shape and size.
        let fixed_layer = crate::hud::ui_size::fixed().filter(|_| !crate::output::hybrid::hud_layer()).map(crate::hud::ui_size::layer_size);
        match fixed_layer {
            Some(size) => crate::hud::ui_size::fill_draw(context, size),
            None => crate::hud::ui_size::band_draw(context),
        }
        let (width, height) = fixed_layer.unwrap_or((width, height));
        // The hands' panels take their pieces' draws here too (the rest go into the layer): the
        // layout is the game's own viewport, unmoved in this path.
        if crate::hud::panels::active() {
            let mut count = 1u32;
            let mut viewport = D3D11_VIEWPORT::default();
            // SAFETY: reads one viewport into a local.
            unsafe { context.RSGetViewports(&mut count, Some(&mut viewport)) };
            if count > 0 {
                crate::hud::panels::note_viewports(viewport);
                PANEL_DRAW.store(true, Relaxed);
            }
        }
        return if crate::output::hybrid::hud_layer() { crate::output::hybrid::hud_targets(context, width, height) } else { crate::output::upscale::hud_targets(context, width, height) };
    }
    let settings = SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
    let settings = settings.as_ref()?;
    let setup_eye = stereo::schedule().map(|s| s.eye_at(frame))?;

    let mut count = 1u32;
    let mut viewport = D3D11_VIEWPORT::default();
    // SAFETY: reads one viewport into a local.
    unsafe { context.RSGetViewports(&mut count, Some(&mut viewport)) };
    if count == 0 {
        return None;
    }
    // A viewport we did not set is the game's own: it becomes the base.
    if !pass.shifted || (viewport.TopLeftX - pass.last_set_x).abs() > 0.01 {
        pass.base = viewport;
        pass.shifted = true;
    }
    let pose = stereo::pending_pose();
    let frustum = stereo::rendered_frustum(&pose, setup_eye)?;
    let base = Rect::new(pass.base.TopLeftX, pass.base.TopLeftY, pass.base.Width, pass.base.Height);
    let rect = Placement::new(frustum, setup_eye, stereo::config().separation_for(&pose), settings.distance).rect(base, base.aspect(), settings.scale);
    let placed = D3D11_VIEWPORT { TopLeftX: rect.x, TopLeftY: rect.y, Width: rect.width, Height: rect.height, ..pass.base };
    pass.last_set_x = placed.TopLeftX;
    // SAFETY: sets one viewport on the game's immediate context, on its own thread.
    unsafe { context.RSSetViewports(Some(&[placed])) };
    crate::hud::ui_size::band_draw(context);
    SHIFTED_DRAWS.fetch_add(1, Relaxed);
    if crate::hud::panels::active() {
        crate::hud::panels::note_viewports(pass.base);
        PANEL_DRAW.store(true, Relaxed);
    }
    None
}

/// Runs a draw once, or, for a HUD draw that belongs in the HUD layer, once into each of its two
/// targets (black and white) and then gives the game its own target binding back. A HUD draw of a
/// piece on the hands goes into its panel instead, either way ([`crate::hud::panels::draw_hud`]).
/// The binding goes through the original `OMSetRenderTargets`, so our own hook does not see it.
fn draw_once_or_into_layer(context: *mut core::ffi::c_void, layer: Option<[ID3D11RenderTargetView; 2]>, draw: impl Fn()) {
    draw_routed(context, layer, draw);
    crate::hud::ui_size::after_draw(context);
}

fn draw_routed(context: *mut core::ffi::c_void, layer: Option<[ID3D11RenderTargetView; 2]>, draw: impl Fn()) {
    if SKIP_DRAW.swap(false, Relaxed) {
        return;
    }
    let as_made = || match &layer {
        Some(targets) => draw_into_layer(context, targets, &draw),
        None => draw(),
    };
    if PANEL_DRAW.swap(false, Relaxed) {
        // A piece's draw goes into its panel as it is; any other the way it would have gone.
        return crate::hud::panels::draw_hud(context, &draw, &as_made);
    }
    as_made()
}

/// One draw into each of the layer's two targets, then the game's own binding back.
fn draw_into_layer(context: *mut core::ffi::c_void, targets: &[ID3D11RenderTargetView; 2], draw: &dyn Fn()) {
    // SAFETY: the game's live context; reads and restores its own bindings around our two draws.
    unsafe {
        let Some(ctx) = ID3D11DeviceContext::from_raw_borrowed(&context) else { return draw() };
        let mut bound = [None];
        let mut depth = None;
        ctx.OMGetRenderTargets(Some(&mut bound), Some(&mut depth));
        let depth_raw = depth.as_ref().map_or(std::ptr::null_mut(), |d| d.as_raw());
        for target in targets {
            let view = [target.as_raw()];
            TARGETS.get()(context, 1, view.as_ptr(), depth_raw);
            draw();
        }
        let game = [bound[0].as_ref().map_or(std::ptr::null_mut(), |v| v.as_raw())];
        TARGETS.get()(context, 1, game.as_ptr(), depth_raw);
    }
    LAYER_DRAWS.fetch_add(1, Relaxed);
}

static LAYER_DRAWS: AtomicU64 = AtomicU64::new(0);

/// A draw call's own arguments (the vertex or index count, the first index or vertex, the base
/// vertex), for the probes ([`crate::hud::panels`]): draws run on the immediate context's one thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawArgs {
    pub indexed: bool,
    pub count: u32,
    pub start: u32,
    pub base: i32,
}

static DRAW_ARGS: Mutex<DrawArgs> = Mutex::new(DrawArgs { indexed: false, count: 0, start: 0, base: 0 });

fn note_draw(args: DrawArgs) {
    if crate::hud::panels::active()
        && let Ok(mut last) = DRAW_ARGS.lock()
    {
        *last = args;
    }
}

/// The arguments of the draw being run.
pub fn draw_args() -> DrawArgs {
    DRAW_ARGS.lock().map(|a| *a).unwrap_or_default()
}
/// The HUD draw just seen stays undrawn ([`crate::hud::panels`]); draws run on the immediate context's
/// one thread, so a flag carries it from the check to the draw.
static SKIP_DRAW: AtomicBool = AtomicBool::new(false);
/// The HUD draw just placed also goes into the HUD panels ([`crate::hud::panels::draw_hud`]).
static PANEL_DRAW: AtomicBool = AtomicBool::new(false);

pub fn layer_draws() -> u64 {
    LAYER_DRAWS.load(Relaxed)
}

pub unsafe extern "system" fn draw_indexed(context: *mut core::ffi::c_void, count: u32, start: u32, base: i32) {
    let _flight = InFlight::enter();
    note_draw(DrawArgs { indexed: true, count, start, base });
    let layer = before_draw(context, count);
    // SAFETY: forwards the game's own call.
    draw_once_or_into_layer(context, layer, || unsafe { DRAW_INDEXED.get()(context, count, start, base) });
}

pub unsafe extern "system" fn draw(context: *mut core::ffi::c_void, count: u32, start: u32) {
    let _flight = InFlight::enter();
    note_draw(DrawArgs { indexed: false, count, start, base: 0 });
    let layer = before_draw(context, count);
    // SAFETY: forwards the game's own call.
    draw_once_or_into_layer(context, layer, || unsafe { DRAW.get()(context, count, start) });
}

pub unsafe extern "system" fn draw_indexed_instanced(context: *mut core::ffi::c_void, count: u32, instances: u32, start: u32, base: i32, first: u32) {
    let _flight = InFlight::enter();
    note_draw(DrawArgs { indexed: true, count, start, base });
    let layer = before_draw(context, count);
    // SAFETY: forwards the game's own call.
    draw_once_or_into_layer(context, layer, || unsafe { DRAW_INDEXED_INSTANCED.get()(context, count, instances, start, base, first) });
}

pub unsafe extern "system" fn draw_instanced(context: *mut core::ffi::c_void, count: u32, instances: u32, start: u32, first: u32) {
    let _flight = InFlight::enter();
    note_draw(DrawArgs { indexed: false, count, start, base: 0 });
    let layer = before_draw(context, count);
    // SAFETY: forwards the game's own call.
    draw_once_or_into_layer(context, layer, || unsafe { DRAW_INSTANCED.get()(context, count, instances, start, first) });
}

/// Where the HUD goes in each eye, as (x, y, width, height) in pixels of a `width` x `height`
/// image: shrunk toward that eye's straight-ahead point and shifted toward the nose.
pub fn eye_rects(pose: &monaka_core::protocol::HeadPose, width: u32, height: u32) -> [[f32; 4]; 2] {
    let settings = SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
    let base = Rect::sized(width as f32, height as f32);
    std::array::from_fn(|eye| match (settings.as_ref(), stereo::rendered_frustum(pose, eye)) {
        (Some(s), Some(frustum)) => Placement::new(frustum, eye, stereo::config().separation_for(pose), s.distance).rect(base, base.aspect(), s.scale).array(),
        _ => base.array(),
    })
}
