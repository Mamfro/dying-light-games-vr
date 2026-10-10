//! The game's UI at a size of our choosing: while VR runs, the HUD and menus keep the size the game
//! had before the switch (the monitor's, 16:9 on most), whatever the eye size: menus stay whole,
//! the crosshair centred, the HUD pieces on the hands. Dying Light 1's UI has its own idea of the
//! screen, apart from the size the world is rendered at, so it never hears of the eye size:
//! - layout (element sizes, positions, font scales) reads the engine's UI screen size, which has an
//!   override the engine itself uses while saving UI packs ([`engine::UI_SIZE_OVERRIDE`]);
//! - three engine UI functions read the video settings' size instead: the UI camera
//!   ([`CAMERA`]), the letterbox placement of HUD screens ([`LETTERBOX`]) and
//!   `IUIElement::PointToScreenSpace`; each runs with that size shown as the fixed one;
//! - the game DLL places things on the UI (markers, prompts) by `IGame::GetScreenWidth` and
//!   `GetScreenHeight`, which answer the fixed size to it;
//! - the camera's screen functions (`IBaseCamera::PointToScreen` and its clamped form, world to
//!   pixels: the crosshair, markers; `GetOnScreenPoint`, `GetOnScreenPointF`, `GetOnScreenVector`,
//!   pixels to the world: the aim and use rays from the screen's centre) read the video settings'
//!   size through another engine global; they too run with the fixed one, so the two directions
//!   and the game's idea of the centre agree (otherwise the crosshair sits at the eye size's
//!   centre).
//!
//! Nothing is laid out again when VR starts or stops: on the monitor the menus are as they were.
//! (Laid out for the square eye instead, the menus come out cut, shifted or squashed, each page its
//! own way: elements scale by width/1280 or height/720 of the `.xui` design, which a square screen
//! sets apart; a wider camera, relayouts and the menu's own resize do not fix that.)
//! The HUD draws go into a layer of the fixed shape ([`layer_size`], [`fill_draw`]), fitted into
//! each eye's HUD place ([`fit_rect`]); without an FSR layer, into a band of that shape in their
//! viewport ([`band_draw`]).

use crate::engine;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Instruction, Original, mem};
use monaka_producer::{Rejection, log};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{D3D11_VIEWPORT, ID3D11DeviceContext};
use windows::core::Interface;

/// The fixed size as `width << 32 | height`; 0 while the UI follows the game's own size.
static FIXED: AtomicU64 = AtomicU64::new(0);
/// The override as the game had it: (set, width, height).
static OVERRIDE_BEFORE: Mutex<Option<(u8, f32, f32)>> = Mutex::new(None);
static ENGINE: AtomicU64 = AtomicU64::new(0);

/// The size the UI is kept at, while it is.
pub fn fixed() -> Option<(u32, u32)> {
    let packed = FIXED.load(Relaxed);
    (packed != 0).then_some(((packed >> 32) as u32, packed as u32))
}

/// The HUD layer's size for the fixed UI: its shape, at most 1440 high (the layer is drawn twice per
/// HUD draw and fitted into part of the eye, so the monitor's 2160 would be spent on nothing).
pub fn layer_size((width, height): (u32, u32)) -> (u32, u32) {
    if height <= 1440 {
        return (width, height);
    }
    ((width as f32 * 1440.0 / height as f32).round() as u32, 1440)
}

/// `rect` (x, y, width, height) narrowed to the fixed shape, centred: where a layer of that shape
/// goes in a place made for a layer of another.
pub fn fit_rect(rect: [f32; 4]) -> [f32; 4] {
    let Some((width, height)) = fixed() else { return rect };
    let [x, y, w, h] = rect;
    let aspect = width as f32 / height as f32;
    if w / h > aspect {
        let fitted = h * aspect;
        [x + (w - fitted) / 2.0, y, fitted, h]
    } else {
        let fitted = w / aspect;
        [x, y + (h - fitted) / 2.0, w, fitted]
    }
}

/// Starts keeping the UI at `size` (the game's size before the switch), the override set before
/// the game applies the eye size.
pub fn begin(engine_base: usize, size: (u32, u32)) {
    if size.0 == 0 || size.1 == 0 {
        return;
    }
    ENGINE.store(engine_base as u64, Relaxed);
    let (flag, value) = (engine_base + engine::UI_SIZE_OVERRIDDEN, engine_base + engine::UI_SIZE_OVERRIDE);
    let mut before = OVERRIDE_BEFORE.lock().unwrap_or_else(|e| e.into_inner());
    if before.is_none() {
        let (Some(set), Some(w), Some(h)) = (mem::read::<u8>(flag), mem::read::<f32>(value), mem::read::<f32>(value + 4)) else { return };
        *before = Some((set, w, h));
    }
    mem::write(value, size.0 as f32);
    mem::write(value + 4, size.1 as f32);
    mem::write(flag, 1u8);
    FIXED.store((size.0 as u64) << 32 | size.1 as u64, Relaxed);
    log!("fixed UI: the game's UI kept at {}x{} (HUD layer {:?})", size.0, size.1, layer_size(size));
}

/// The UI follows the game's own size again.
pub fn end() {
    if FIXED.swap(0, Relaxed) == 0 {
        return;
    }
    let base = ENGINE.load(Relaxed) as usize;
    if let Some((set, w, h)) = OVERRIDE_BEFORE.lock().unwrap_or_else(|e| e.into_inner()).take() {
        mem::write(base + engine::UI_SIZE_OVERRIDE, w);
        mem::write(base + engine::UI_SIZE_OVERRIDE + 4, h);
        mem::write(base + engine::UI_SIZE_OVERRIDDEN, set);
    }
    log!("fixed UI: the game's UI follows its own size again");
}

// The engine's UI functions that read the video settings' size (`[[game + 8] + 0xC0]`; these are
// all three readers of it in the UI code).
/// The UI camera: `(ui system, camera, fov)`, engine+0x5ddc00 (`OnResolutionChange`, `LoadUIPack`,
/// `SetUICameraFov` and others make it): fov 45 degrees, centred on the screen in its pixels.
const CAMERA: usize = 0x5DDC00;
const CAMERA_PROLOGUE: [Instruction; 2] = [Instruction::plain(&[0x48, 0x85, 0xd2]), Instruction::rip_relative(&[0x0f, 0x84, 0x5c, 0x01, 0x00, 0x00], 2)]; // test rdx, rdx; je
type CameraFn = unsafe extern "system" fn(usize, usize, f32);
/// A HUD screen's letterbox placement: `(element, ?, size, axis)`, engine+0x5e9330.
const LETTERBOX: usize = 0x5E9330;
const LETTERBOX_PROLOGUE: [Instruction; 1] = [Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]; // mov [rsp+8], rbx
type LetterboxFn = unsafe extern "system" fn(usize, usize, usize, i32);
const POINT_TO_SCREEN: &str = "?PointToScreenSpace@IUIElement@@QEAAXAEBVvec3@@AEAV2@@Z";
const POINT_TO_SCREEN_PROLOGUE: [Instruction; 2] = [Instruction::plain(&[0x48, 0x83, 0xec, 0x18]), Instruction::plain(&[0x48, 0x8b, 0x41, 0x18])]; // sub rsp, 0x18; mov rax, [rcx+0x18]
type PointFn = unsafe extern "system" fn(usize, usize, usize);
const SCREEN_WIDTH: &str = "?GetScreenWidth@IGame@@QEAAHXZ";
const SCREEN_HEIGHT: &str = "?GetScreenHeight@IGame@@QEAAHXZ";
type ScreenSideFn = unsafe extern "system" fn(usize) -> i32;
// The camera's screen functions: (camera, result, ...) -> result.
const TO_SCREEN: &str = "?PointToScreen@IBaseCamera@@QEAA?BVvec2@@AEBVvec3@@@Z";
const TO_SCREEN_CLAMPED: &str = "?PointToScreenClampToFrustum@IBaseCamera@@QEAA?BVvec3@@AEBV2@@Z";
const TO_SCREEN_PROLOGUE: [Instruction; 2] = [Instruction::plain(&[0x48, 0x83, 0xec, 0x18]), Instruction::plain(&[0xf3, 0x41, 0x0f, 0x10, 0x58, 0x04])]; // sub rsp, 0x18; movss xmm3, [r8+4]
const TO_SCREEN_CLAMPED_PROLOGUE: [Instruction; 2] = [Instruction::plain(&[0x48, 0x83, 0xec, 0x18]), Instruction::plain(&[0xf3, 0x41, 0x0f, 0x10, 0x58, 0x08])]; // sub rsp, 0x18; movss xmm3, [r8+8]
type ToScreenFn = unsafe extern "system" fn(usize, usize, usize) -> usize;
const ON_SCREEN_VECTOR: &str = "?GetOnScreenVector@IBaseCamera@@QEAA?BVvec3@@HH@Z";
const ON_SCREEN_VECTOR_PROLOGUE: [Instruction; 2] = [Instruction::plain(&[0x48, 0x83, 0xec, 0x48]), Instruction::plain(&[0x4c, 0x8b, 0x51, 0x08])]; // sub rsp, 0x48; mov r10, [rcx+8]
const ON_SCREEN_POINT: &str = "?GetOnScreenPoint@IBaseCamera@@QEAA?BVvec3@@HH@Z";
const ON_SCREEN_POINT_PROLOGUE: [Instruction; 2] = [Instruction::plain(&[0x48, 0x83, 0xec, 0x18]), Instruction::rip_relative(&[0x48, 0x8b, 0x05, 0x35, 0x0d, 0x91, 0x00], 3)]; // sub rsp, 0x18; mov rax, [rip+]
type OnScreenFn = unsafe extern "system" fn(usize, usize, i32, i32) -> usize;
const ON_SCREEN_POINT_F: &str = "?GetOnScreenPointF@IBaseCamera@@QEAA?BVvec3@@MM@Z";
const ON_SCREEN_POINT_F_PROLOGUE: [Instruction; 2] = [Instruction::plain(&[0x48, 0x83, 0xec, 0x18]), Instruction::rip_relative(&[0x48, 0x8b, 0x05, 0x15, 0x0c, 0x91, 0x00], 3)]; // sub rsp, 0x18; mov rax, [rip+]
type OnScreenFFn = unsafe extern "system" fn(usize, usize, f32, f32) -> usize;

static CAMERA_ORIGINAL: Original<CameraFn> = Original::new();
static LETTERBOX_ORIGINAL: Original<LetterboxFn> = Original::new();
static POINT_ORIGINAL: Original<PointFn> = Original::new();
static WIDTH_ORIGINAL: Original<ScreenSideFn> = Original::new();
static HEIGHT_ORIGINAL: Original<ScreenSideFn> = Original::new();
static TO_SCREEN_ORIGINAL: Original<ToScreenFn> = Original::new();
static TO_SCREEN_CLAMPED_ORIGINAL: Original<ToScreenFn> = Original::new();
static ON_SCREEN_VECTOR_ORIGINAL: Original<OnScreenFn> = Original::new();
static ON_SCREEN_POINT_ORIGINAL: Original<OnScreenFn> = Original::new();
static ON_SCREEN_POINT_F_ORIGINAL: Original<OnScreenFFn> = Original::new();

/// Runs `call` with the video settings' applied size shown as the fixed one, then puts it back (the
/// call as it is while the UI is not fixed). The three readers run on the game's thread; nothing
/// else of the UI reads it meanwhile. The switch's own relayout goes through here too: it runs
/// before the hooks are in.
///
/// Should two threads ever overlap here, the second finds the fixed size in place: it puts back
/// the real size last seen ([`REAL`]), not what it found, so the settings always end real.
pub fn as_fixed(call: impl FnOnce()) {
    let settings = fixed().and_then(|_| eng_chr::game::game()).and_then(|g| eng_chr::game::video_settings(g, engine::VIDEO_SETTINGS_IN_GAME));
    let (Some((width, height)), Some(settings)) = (fixed(), settings) else { return call() };
    let at = settings + engine::VIDEO_APPLIED;
    let (Some(found_width), Some(found_height)) = (mem::read::<u32>(at), mem::read::<u32>(at + 4)) else { return call() };
    let (real_width, real_height) = if (found_width, found_height) == (width, height) {
        let packed = REAL.load(Relaxed);
        if packed == 0 {
            return call();
        }
        ((packed >> 32) as u32, packed as u32)
    } else {
        REAL.store((found_width as u64) << 32 | found_height as u64, Relaxed);
        (found_width, found_height)
    };
    mem::write(at, width);
    mem::write(at + 4, height);
    call();
    mem::write(at, real_width);
    mem::write(at + 4, real_height);
}

/// The video settings' applied size last seen unswapped (`width << 32 | height`).
static REAL: AtomicU64 = AtomicU64::new(0);

unsafe extern "system" fn camera(system: usize, camera: usize, fov: f32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    as_fixed(|| unsafe { CAMERA_ORIGINAL.get()(system, camera, fov) });
}

unsafe extern "system" fn letterbox(element: usize, b: usize, size: usize, axis: i32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    as_fixed(|| unsafe { LETTERBOX_ORIGINAL.get()(element, b, size, axis) });
}

unsafe extern "system" fn point_to_screen(element: usize, point: usize, out: usize) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    as_fixed(|| unsafe { POINT_ORIGINAL.get()(element, point, out) });
}

unsafe extern "system" fn screen_width(game: usize) -> i32 {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    fixed().map_or_else(|| unsafe { WIDTH_ORIGINAL.get()(game) }, |(w, _)| w as i32)
}

unsafe extern "system" fn screen_height(game: usize) -> i32 {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    fixed().map_or_else(|| unsafe { HEIGHT_ORIGINAL.get()(game) }, |(_, h)| h as i32)
}

/// `call`'s result, run as [`as_fixed`] runs it.
fn as_fixed_returning<R: Default>(call: impl FnOnce() -> R) -> R {
    let mut result = R::default();
    as_fixed(|| result = call());
    result
}

unsafe extern "system" fn to_screen(camera: usize, out: usize, point: usize) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    as_fixed_returning(|| unsafe { TO_SCREEN_ORIGINAL.get()(camera, out, point) })
}

unsafe extern "system" fn to_screen_clamped(camera: usize, out: usize, point: usize) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    as_fixed_returning(|| unsafe { TO_SCREEN_CLAMPED_ORIGINAL.get()(camera, out, point) })
}

unsafe extern "system" fn on_screen_vector(camera: usize, out: usize, x: i32, y: i32) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    as_fixed_returning(|| unsafe { ON_SCREEN_VECTOR_ORIGINAL.get()(camera, out, x, y) })
}

unsafe extern "system" fn on_screen_point(camera: usize, out: usize, x: i32, y: i32) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    as_fixed_returning(|| unsafe { ON_SCREEN_POINT_ORIGINAL.get()(camera, out, x, y) })
}

unsafe extern "system" fn on_screen_point_f(camera: usize, out: usize, x: f32, y: f32) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    as_fixed_returning(|| unsafe { ON_SCREEN_POINT_F_ORIGINAL.get()(camera, out, x, y) })
}

/// Hooks the UI's readers of the screen size (each passes through while the UI is not fixed).
pub fn install(hooks: &mut Hooks, engine_module: &Module, gamedll: &Module) -> Result<(), Rejection> {
    let export = |name: &str| engine_module.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    let point = export(POINT_TO_SCREEN)?;
    let (to, clamped, vector, on_point, on_point_f) = (export(TO_SCREEN)?, export(TO_SCREEN_CLAMPED)?, export(ON_SCREEN_VECTOR)?, export(ON_SCREEN_POINT)?, export(ON_SCREEN_POINT_F)?);
    // SAFETY: each target is checked against its exact prologue (the engine build is hash
    // checked); each detour has the target's signature; the imports are the engine's exports of
    // `int IGame::GetScreenWidth()` and `GetScreenHeight()`.
    unsafe {
        hooks.inline(&CAMERA_ORIGINAL, "UI camera", engine_module.at(CAMERA), &CAMERA_PROLOGUE, camera as CameraFn)?;
        hooks.inline(&LETTERBOX_ORIGINAL, "UI letterbox", engine_module.at(LETTERBOX), &LETTERBOX_PROLOGUE, letterbox as LetterboxFn)?;
        hooks.inline(&POINT_ORIGINAL, "UI point to screen", point, &POINT_TO_SCREEN_PROLOGUE, point_to_screen as PointFn)?;
        hooks.import(&WIDTH_ORIGINAL, "screen width", gamedll, engine::ENGINE, SCREEN_WIDTH, screen_width as ScreenSideFn)?;
        hooks.import(&HEIGHT_ORIGINAL, "screen height", gamedll, engine::ENGINE, SCREEN_HEIGHT, screen_height as ScreenSideFn)?;
        hooks.inline(&TO_SCREEN_ORIGINAL, "camera point to screen", to, &TO_SCREEN_PROLOGUE, to_screen as ToScreenFn)?;
        hooks.inline(&TO_SCREEN_CLAMPED_ORIGINAL, "camera point to screen (clamped)", clamped, &TO_SCREEN_CLAMPED_PROLOGUE, to_screen_clamped as ToScreenFn)?;
        hooks.inline(&ON_SCREEN_VECTOR_ORIGINAL, "camera on-screen vector", vector, &ON_SCREEN_VECTOR_PROLOGUE, on_screen_vector as OnScreenFn)?;
        hooks.inline(&ON_SCREEN_POINT_ORIGINAL, "camera on-screen point", on_point, &ON_SCREEN_POINT_PROLOGUE, on_screen_point as OnScreenFn)?;
        hooks.inline(&ON_SCREEN_POINT_F_ORIGINAL, "camera on-screen point (float)", on_point_f, &ON_SCREEN_POINT_F_PROLOGUE, on_screen_point_f as OnScreenFFn)?;
    }
    log!("fixed UI: watching the UI's readers of the screen size");
    Ok(())
}

/// The viewport and scissor a moved draw had, put back after it ([`after_draw`]).
static MOVED: Mutex<Option<(D3D11_VIEWPORT, Option<RECT>)>> = Mutex::new(None);
static BANDS: AtomicU64 = AtomicU64::new(0);

/// A HUD draw into the fixed UI's own layer: its whole `size`, the game's viewport and scissor
/// scaled into it.
pub fn fill_draw(context: &ID3D11DeviceContext, size: (u32, u32)) {
    let mut count = 1u32;
    let mut viewport = D3D11_VIEWPORT::default();
    // SAFETY: reads one viewport into a local.
    unsafe { context.RSGetViewports(&mut count, Some(&mut viewport)) };
    if count == 0 || viewport.Width <= 0.0 || viewport.Height <= 0.0 {
        return;
    }
    let full = D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: size.0 as f32, Height: size.1 as f32, ..viewport };
    set_view(context, viewport, full);
}

/// The game's HUD viewport `base` narrowed to a band of the fixed shape, the space the HUD is laid
/// out in when it is placed in the eye, with the bound scissor (in `base`'s pixels) carried into the
/// band. `bound`, the viewport bound now, is given back after the draw ([`after_draw`]). `base` as it
/// is while the UI is not fixed or is no narrower. The hand panels map from this band too, so their
/// pieces and the game's clipping agree.
pub fn layout_band(context: &ID3D11DeviceContext, bound: D3D11_VIEWPORT, base: D3D11_VIEWPORT) -> D3D11_VIEWPORT {
    let Some((width, height)) = fixed() else { return base };
    let aspect = width as f32 / height as f32;
    if base.Width <= 0.0 || base.Height <= 0.0 || base.Width / base.Height >= aspect - 0.01 {
        return base;
    }
    let band_height = base.Width / aspect;
    let band = D3D11_VIEWPORT { TopLeftY: base.TopLeftY + (base.Height - band_height) / 2.0, Height: band_height, ..base };
    let game_scissor = move_scissor(context, base, band);
    *MOVED.lock().unwrap_or_else(|e| e.into_inner()) = Some((bound, game_scissor));
    if BANDS.fetch_add(1, Relaxed) == 0 {
        log!("fixed UI: the HUD is laid out in the band {:.0},{:.0} {:.0}x{:.0} of its viewport, then placed", band.TopLeftX, band.TopLeftY, band.Width, band.Height);
    }
    band
}

/// A HUD draw into the hybrid's HUD layer: into a band of the fixed shape in the viewport bound
/// now, when that viewport is narrower.
pub fn band_draw(context: &ID3D11DeviceContext) {
    let Some((width, height)) = fixed() else { return };
    let aspect = width as f32 / height as f32;
    let mut count = 1u32;
    let mut viewport = D3D11_VIEWPORT::default();
    // SAFETY: reads one viewport into a local.
    unsafe { context.RSGetViewports(&mut count, Some(&mut viewport)) };
    if count == 0 || viewport.Width <= 0.0 || viewport.Height <= 0.0 || viewport.Width / viewport.Height >= aspect - 0.01 {
        return;
    }
    let band_height = viewport.Width / aspect;
    let band = D3D11_VIEWPORT { TopLeftY: viewport.TopLeftY + (viewport.Height - band_height) / 2.0, Height: band_height, ..viewport };
    set_view(context, viewport, band);
    if BANDS.fetch_add(1, Relaxed) == 0 {
        log!("fixed UI: the HUD draws go into the band {:.0},{:.0} {:.0}x{:.0} of their viewport", band.TopLeftX, band.TopLeftY, band.Width, band.Height);
    }
}

/// Moves the draw from the game's viewport `from` to `to`: the viewport, and the bound scissor
/// rectangle (the game clips text boxes with one, in its viewport's pixels) carried along; both
/// put back after the draw ([`after_draw`]).
fn set_view(context: &ID3D11DeviceContext, from: D3D11_VIEWPORT, to: D3D11_VIEWPORT) {
    let game_scissor = move_scissor(context, from, to);
    // SAFETY: sets one viewport on the game's immediate context, on its own thread; put back after
    // the draw.
    unsafe { context.RSSetViewports(Some(&[to])) };
    *MOVED.lock().unwrap_or_else(|e| e.into_inner()) = Some((from, game_scissor));
}

/// The bound scissor rectangle carried from the viewport `from` into `to`; the game's own, for
/// [`after_draw`] to give back.
fn move_scissor(context: &ID3D11DeviceContext, from: D3D11_VIEWPORT, to: D3D11_VIEWPORT) -> Option<RECT> {
    let mut count = 1u32;
    let mut scissor = [RECT::default()];
    // SAFETY: reads one scissor rectangle into a local.
    unsafe { context.RSGetScissorRects(&mut count, Some(scissor.as_mut_ptr())) };
    let game_scissor = (count > 0).then_some(scissor[0]);
    let (sx, sy) = (to.Width / from.Width, to.Height / from.Height);
    let x = |v: i32| ((v as f32 - from.TopLeftX) * sx + to.TopLeftX).round() as i32;
    let y = |v: i32| ((v as f32 - from.TopLeftY) * sy + to.TopLeftY).round() as i32;
    if let Some(s) = game_scissor {
        // SAFETY: sets one scissor on the game's immediate context, on its own thread; put back
        // after the draw.
        unsafe { context.RSSetScissorRects(Some(&[RECT { left: x(s.left), top: y(s.top), right: x(s.right), bottom: y(s.bottom) }])) };
    }
    game_scissor
}

/// After a draw: the viewport and scissor a moved draw had, back.
pub fn after_draw(context: *mut core::ffi::c_void) {
    let Some((viewport, scissor)) = MOVED.lock().ok().and_then(|mut b| b.take()) else { return };
    // SAFETY: the game's live context, on its own thread.
    if let Some(ctx) = unsafe { ID3D11DeviceContext::from_raw_borrowed(&context) } {
        // SAFETY: gives the game its own viewport and scissor back.
        unsafe {
            ctx.RSSetViewports(Some(&[viewport]));
            if let Some(s) = scissor {
                ctx.RSSetScissorRects(Some(&[s]));
            }
        }
    }
}
