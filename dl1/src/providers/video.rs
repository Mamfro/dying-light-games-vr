//! Provider: video (`monaka_stereo::eye_size::Resizes`), asking for a render size and putting the
//! game's back.
//!
//! Rendering at the headset's eye size while VR runs, as DL2 does: the start asks the game's own
//! video settings for `eye_size` and waits for the swapchain to follow, before anything sized
//! from the back buffer exists; the stop asks for the original size back. Layout in
//! [`engine`](crate::providers::engine) (`VIDEO_*`).
//!
//! The game applies an asked mode at the top of its loop, on its thread
//! ([`engine::APPLY_VIDEO_SETTINGS`]); hooked from the switch to the stop, the levels' UI is made
//! for the new size right there ([`applied`]).

use crate::Dl1;
use crate::game_swapchain;
use crate::providers::engine::{self, ApplyVideoSettingsFn};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::game::{Lack, Start};
use monaka_producer::log;
use monaka_stereo::eye_size::{Ask, Resizes};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering::*};
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;

impl Resizes for Dl1 {
    fn ask(_: (u32, u32), _: (u32, u32)) -> Ask {
        Ask::Cannot("switched at the start only".into())
    }

    /// The game applies it on its next frame; its levels then lay their menus out again.
    fn put_back() {
        restore_and_relayout();
    }
}

/// The eye size component's install (`crate::EYE_SIZE`): the game switched to the eye size (with
/// FSR, the size the upscaler renders at), waited for.
pub(crate) fn install_eye_size(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = crate::config(start);
    let Some(size) = config.eye_size else { return Ok(()) };
    let render = config.upscaled().map_or(size, |fsr| fsr.render_size(size));
    if let Err(why) = switch_to(&start.build.engine, &game_swapchain(&start.build.renderer)?, render, Duration::from_secs(8)) {
        log!("rendering at the game's own size: {why}");
    }
    Ok(())
}

/// The settings object and the requested mode it had before the switch.
static SWITCHED: Mutex<Option<(usize, [u8; engine::VIDEO_MODE_SIZE])>> = Mutex::new(None);
/// The size the game had before this run switched it (width << 32 | height; 0: not switched).
static BEFORE: AtomicU64 = AtomicU64::new(0);

/// The size the game had before this run switched it, while the switch holds.
pub fn screen_before() -> Option<(u32, u32)> {
    let packed = BEFORE.load(Relaxed);
    (packed != 0).then_some(((packed >> 32) as u32, packed as u32))
}

fn switched(settings: usize, original: [u8; engine::VIDEO_MODE_SIZE]) {
    let width = u32::from_le_bytes(original[0..4].try_into().expect("4 bytes"));
    let height = u32::from_le_bytes(original[4..8].try_into().expect("4 bytes"));
    BEFORE.store((width as u64) << 32 | height as u64, Relaxed);
    *SWITCHED.lock().unwrap_or_else(|e| e.into_inner()) = Some((settings, original));
}

fn size_of(chain: &IDXGISwapChain) -> Option<(u32, u32)> {
    // SAFETY: COM call on the game's live swapchain (DXGI locks internally).
    let desc = unsafe { chain.GetDesc() }.ok()?;
    Some((desc.BufferDesc.Width, desc.BufferDesc.Height))
}

/// The size of the screen the game's window is on.
fn screen_size(chain: &IDXGISwapChain) -> Option<(u32, u32)> {
    // SAFETY: COM calls on the game's live swapchain and its output.
    let desc = unsafe { chain.GetContainingOutput().ok()?.GetDesc() }.ok()?;
    let r = desc.DesktopCoordinates;
    Some(((r.right - r.left) as u32, (r.bottom - r.top) as u32))
}

fn request(settings: usize, mode: &[u8; engine::VIDEO_MODE_SIZE]) {
    for (i, &byte) in mode.iter().enumerate() {
        mem::write(settings + engine::VIDEO_REQUESTED + i, byte);
    }
    mem::write(settings + engine::VIDEO_FORCE_APPLY, 1u8);
    mem::write(settings + engine::VIDEO_APPLY_PENDING, 1u8);
}

/// Switches the game to `width`x`height` through its video settings and waits for the swapchain
/// to show it (the game must be running). Gives up after `wait`, putting the request back.
pub fn switch_to(engine_module: &Module, chain: &IDXGISwapChain, (width, height): (u32, u32), wait: Duration) -> Result<(), String> {
    let current = size_of(chain).ok_or("swapchain description unavailable")?;
    let settings = engine::video_settings(engine_module).ok_or("no video settings object (the game object is not made yet)")?;
    watch_applies(engine_module)?;
    if current == (width, height) {
        keep_size(settings, (width, height))?;
        // The game applies the mode it has, and its levels' UI is made for it.
        RELAYOUT.store(LAY_OUT, Release);
        mem::write(settings + engine::VIDEO_APPLY_PENDING, 1u8);
        return Ok(());
    }
    let applied = (mem::read::<u32>(settings + engine::VIDEO_APPLIED), mem::read::<u32>(settings + engine::VIDEO_APPLIED + 4));
    // A run that never asked for the size back (the game crashed or was killed) leaves the eye size
    // in the settings, and the game saves it: started with it, a borderless window that tall is
    // clamped to the screen and the square view is drawn squashed into it. Asked again, the game
    // applies it as it does in play; the size handed back at the end is then the screen's.
    let leftover = applied == (Some(width), Some(height));
    if applied != (Some(current.0), Some(current.1)) && !leftover {
        return Err(format!("video settings hold {applied:?}, not the swapchain's {current:?}"));
    }
    let mut original = [0u8; engine::VIDEO_MODE_SIZE];
    if !mem::read_bytes(settings + engine::VIDEO_REQUESTED, &mut original) {
        return Err("requested mode unreadable".into());
    }
    if leftover {
        let (screen_width, screen_height) = screen_size(chain).ok_or("the settings hold the eye size already and the screen size is unknown")?;
        original[0..4].copy_from_slice(&screen_width.to_le_bytes());
        original[4..8].copy_from_slice(&screen_height.to_le_bytes());
        log!("the settings hold {width}x{height} from a run that did not hand the size back, the swapchain {}x{}: asking again; {screen_width}x{screen_height} (the screen) at the end", current.0, current.1);
    }
    let back = (u32::from_le_bytes(original[0..4].try_into().expect("4 bytes")), u32::from_le_bytes(original[4..8].try_into().expect("4 bytes")));
    crate::extras::ui_size::begin(engine_module.at(0), back);
    let mut mode = original;
    mode[0..4].copy_from_slice(&width.to_le_bytes());
    mode[4..8].copy_from_slice(&height.to_le_bytes());
    RELAYOUT.store(LAY_OUT, Release);
    request(settings, &mode);
    switched(settings, original);
    log!("requested {width}x{height} (was {}x{})", current.0, current.1);
    let started = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let now = size_of(chain);
        if now == Some((width, height)) {
            log!("swapchain at {width}x{height} after {} ms", started.elapsed().as_millis());
            return Ok(());
        }
        if started.elapsed() >= wait {
            RELAYOUT.store(0, Release);
            restore();
            return Err(format!("the swapchain is still {now:?} after {} ms; size request withdrawn", wait.as_millis()));
        }
    }
}

/// The swapchain is at `size` already. A run stopped moments before can have asked for its own
/// size back without the game applying it yet: the eye size is then still on, and a few seconds
/// into this run the game goes back to the monitor's size under the upscaler, so nothing reaches
/// the headset. That request is taken over: this size asked for again, and the one it held handed
/// back at this run's end.
fn keep_size(settings: usize, (width, height): (u32, u32)) -> Result<(), String> {
    let mut requested = [0u8; engine::VIDEO_MODE_SIZE];
    if !mem::read_bytes(settings + engine::VIDEO_REQUESTED, &mut requested) {
        return Err("requested mode unreadable".into());
    }
    let wanted = (u32::from_le_bytes(requested[0..4].try_into().expect("4 bytes")), u32::from_le_bytes(requested[4..8].try_into().expect("4 bytes")));
    if wanted == (width, height) {
        return Ok(());
    }
    let mut mode = requested;
    mode[0..4].copy_from_slice(&width.to_le_bytes());
    mode[4..8].copy_from_slice(&height.to_le_bytes());
    for (i, &byte) in mode.iter().enumerate() {
        mem::write(settings + engine::VIDEO_REQUESTED + i, byte);
    }
    switched(settings, requested);
    log!("the game is at {width}x{height} with {}x{} still asked for (the last run's hand-back): {width}x{height} asked for again, {}x{} at the end", wanted.0, wanted.1, wanted.0, wanted.1);
    Ok(())
}

/// What the next applied mode does to the levels' UI: nothing, [`LAY_OUT`] (each level's UI
/// manager makes its projection again from the size now in the video settings), or
/// [`PLACE_AGAIN`] (also its screens placed against the screen's edges again,
/// [`engine::UI_HOLDER_AUTO_LAYOUT`], which the stop asks for after the camera is made for the
/// monitor).
static RELAYOUT: AtomicU8 = AtomicU8::new(0);
const LAY_OUT: u8 = 1;
const PLACE_AGAIN: u8 = 2;
/// The modes the game applied while watched.
static APPLIES: AtomicU64 = AtomicU64::new(0);
/// `IUIManager::OnResolutionChange`, resolved for the game's thread, and the engine's base.
static ON_CHANGE: AtomicUsize = AtomicUsize::new(0);
static ENGINE_BASE: AtomicUsize = AtomicUsize::new(0);

static APPLY_ORIGINAL: Original<ApplyVideoSettingsFn> = Original::new();
/// The apply's hook, from the switch to the stop (the run's own hooks are gone by the time the
/// stop's mode is applied).
static WATCH: Mutex<Option<Hooks>> = Mutex::new(None);

/// Hooks the game's apply of an asked mode, once.
fn watch_applies(engine_module: &Module) -> Result<(), String> {
    let mut watch = WATCH.lock().unwrap_or_else(|e| e.into_inner());
    if watch.is_some() {
        return Ok(());
    }
    let on_change = engine_module.export(engine::UI_ON_RESOLUTION_CHANGE).ok_or("the engine does not export IUIManager::OnResolutionChange")?;
    ON_CHANGE.store(on_change, Relaxed);
    ENGINE_BASE.store(engine_module.at(0), Relaxed);
    let mut hooks = Hooks::default();
    hooks.site(&APPLY_ORIGINAL, engine_module, &engine::APPLY_VIDEO_SETTINGS, applied).map_err(|e| e.to_string())?;
    hooks.enable().map_err(|e| e.to_string())?;
    *watch = Some(hooks);
    Ok(())
}

fn unwatch() {
    if let Some(mut hooks) = WATCH.lock().unwrap_or_else(|e| e.into_inner()).take()
        && let Err(e) = hooks.disable()
    {
        log!("video settings: unhooking failed: {e}");
    }
}

/// The game's apply of an asked mode, on its thread at the top of its loop: once a pending mode is
/// applied, the levels' UI is made for it as [`RELAYOUT`] asks. The game does not tell its levels'
/// UI when the video settings change size (`engine.rs`), so the UI would keep the projection of
/// the size it was made at: the pause menu then sits shrunk in a corner, in VR and on the monitor
/// after it.
unsafe extern "system" fn applied(settings: usize) -> usize {
    let _flight = InFlight::enter();
    let pending = mem::read::<u8>(settings + engine::VIDEO_APPLY_PENDING).is_some_and(|p| p != 0);
    // SAFETY: forwards the game's own call.
    let result = unsafe { APPLY_ORIGINAL.get()(settings) };
    if pending {
        let how = RELAYOUT.swap(0, AcqRel);
        if how != 0 {
            log!("menus laid out again for the applied mode: {}", relayout(how == PLACE_AGAIN));
        }
        APPLIES.fetch_add(1, Release);
    }
    result
}

/// On the game's thread: each level's UI manager makes its projection again, as the game's own
/// handler would (not called: run from outside it breaks the game, `engine.rs`). While VR runs
/// the UI keeps the monitor's size (`extras::ui_size`), so this makes each camera of that size.
fn relayout(place_again: bool) -> String {
    let engine_base = ENGINE_BASE.load(Relaxed);
    // SAFETY: the engine's export, with this type (`engine.rs`).
    let on_change = unsafe { std::mem::transmute::<usize, engine::UiOnResolutionChangeFn>(ON_CHANGE.load(Relaxed)) };
    let levels = engine::game_at(engine_base)
        .and_then(|game| mem::read::<usize>(game + 8))
        .and_then(|g| Some((mem::read::<usize>(g + engine::GAME_LEVELS)?, mem::read::<i32>(g + engine::GAME_LEVEL_COUNT)?)));
    let Some((array, count)) = levels.filter(|&(a, c)| a != 0 && (0..=64).contains(&c)) else {
        return "the level list was unreadable".into();
    };
    let mut notes = Vec::new();
    let mut done = 0;
    for i in 0..count as usize {
        let Some(level) = mem::read::<usize>(array + 8 * i).filter(|&l| l != 0) else {
            notes.push("none".to_owned());
            continue;
        };
        // The array holds the levels themselves (`CLevel`), what an `ILevel` keeps at +8: the UI
        // manager is `[[level + 0x568] + 0x1770]`, each step checked, and the manager's own first
        // field, which the change reads.
        let holder = mem::read::<usize>(level + engine::LEVEL_UI_HOLDER).filter(|&p| p != 0);
        let manager = holder.and_then(|h| mem::read::<usize>(h + engine::UI_HOLDER_MANAGER)).filter(|&m| m != 0);
        let inner = manager.and_then(mem::read::<usize>).filter(|&p| p != 0);
        match (holder, manager, inner) {
            (Some(holder), Some(manager), Some(_)) => {
                if place_again {
                    let layout = holder + engine::UI_HOLDER_AUTO_LAYOUT;
                    if let Some(flags) = mem::read::<u8>(layout) {
                        mem::write(layout, flags | engine::AUTO_LAYOUT_DIRTY);
                    }
                }
                // SAFETY: the engine's own call on its live level's UI manager, on the game's
                // thread, as the game's handler makes it (at the fixed UI's size while VR runs,
                // `extras::ui_size`).
                crate::extras::ui_size::as_fixed(|| unsafe { on_change(manager as *mut core::ffi::c_void) });
                notes.push("laid out".to_owned());
                done += 1;
            }
            (None, ..) => notes.push("no UI holder".to_owned()),
            (_, None, _) => notes.push("no UI manager".to_owned()),
            _ => notes.push("an empty UI manager".to_owned()),
        }
    }
    format!("{done} of {count} levels' UI ({})", notes.join(", "))
}

/// [`restore`], the levels' UI made for that size and placed again as the game applies it, then
/// the apply unhooked (the stop).
pub fn restore_and_relayout() {
    if screen_before().is_some() {
        let before = APPLIES.load(Acquire);
        RELAYOUT.store(PLACE_AGAIN, Release);
        restore();
        let started = Instant::now();
        while APPLIES.load(Acquire) == before && started.elapsed() < Duration::from_millis(1500) {
            std::thread::sleep(Duration::from_millis(10));
        }
        if APPLIES.load(Acquire) == before {
            RELAYOUT.store(0, Release);
            log!("menus not laid out again: the game applied no mode in 1.5 s (it applies it while it runs)");
        }
    } else {
        restore();
    }
    unwatch();
}

/// Asks the game for the mode it had before [`switch_to`], if that changed it.
pub fn restore() {
    crate::extras::ui_size::end();
    BEFORE.store(0, Relaxed);
    if let Some((settings, original)) = SWITCHED.lock().unwrap_or_else(|e| e.into_inner()).take() {
        request(settings, &original);
        let width = u32::from_le_bytes(original[0..4].try_into().expect("4 bytes"));
        let height = u32::from_le_bytes(original[4..8].try_into().expect("4 bytes"));
        log!("requested the original {width}x{height} back");
    }
}
