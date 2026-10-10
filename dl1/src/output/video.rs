//! Rendering at the headset's eye size while VR runs, as DL2 does: the start asks the game's own
//! video settings for `eye_size` and waits for the swapchain to follow, before anything sized
//! from the back buffer exists; the stop asks for the original size back. Layout in
//! [`engine`](crate::engine) (`VIDEO_*`); `probe_video` (`research::video`) looks for it.

use crate::engine;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, mem};
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::*};
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;

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

fn settings_of(game: usize) -> Option<usize> {
    eng_chr::game::video_settings(game, engine::VIDEO_SETTINGS_IN_GAME)
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
/// to show it (the game must be running, not paused). Gives up after `wait`, putting the request
/// back. `gamedll` hooks are enabled only for the duration.
pub fn switch_to(gamedll: &Module, chain: &IDXGISwapChain, (width, height): (u32, u32), wait: Duration) -> Result<(), String> {
    let current = size_of(chain).ok_or("swapchain description unavailable")?;
    let mut hooks = Hooks::default();
    // SAFETY: the export's type is `float IGame::GetGameTimeDelta() const`.
    unsafe { eng_chr::game::hook(&mut hooks, gamedll) }.map_err(|e| e.to_string())?;
    hooks.enable().map_err(|e| e.to_string())?;
    let game = eng_chr::game::wait_for_game(Duration::from_secs(2));
    hooks.disable().map_err(|e| e.to_string())?;
    let Some(game) = game else {
        if current == (width, height) {
            return Ok(());
        }
        return Err("the game made no frame in two seconds (paused, or in a loading screen?)".into());
    };
    let settings = settings_of(game).ok_or("no video settings object")?;
    if current == (width, height) {
        keep_size(settings, (width, height))?;
        relayout(gamedll, chain, (width, height), Duration::from_secs(1), false);
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
    if let Ok(engine_module) = monaka_producer::require_build(engine::ENGINE, engine::ENGINE_SHA256) {
        let back = (u32::from_le_bytes(original[0..4].try_into().expect("4 bytes")), u32::from_le_bytes(original[4..8].try_into().expect("4 bytes")));
        crate::hud::ui_size::begin(engine_module.at(0), back);
    }
    let mut mode = original;
    mode[0..4].copy_from_slice(&width.to_le_bytes());
    mode[4..8].copy_from_slice(&height.to_le_bytes());
    request(settings, &mode);
    switched(settings, original);
    log!("requested {width}x{height} (was {}x{})", current.0, current.1);
    let started = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let now = size_of(chain);
        if now == Some((width, height)) {
            log!("swapchain at {width}x{height} after {} ms", started.elapsed().as_millis());
            relayout(gamedll, chain, (width, height), Duration::from_secs(1), false);
            return Ok(());
        }
        if started.elapsed() >= wait {
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

/// At the stop: each level's screens also placed against the monitor's edges again
/// ([`engine::UI_HOLDER_AUTO_LAYOUT`]), after their camera is made for it.
static PLACE_AGAIN: AtomicBool = AtomicBool::new(false);

/// The levels' UI laid out again: pending for the game's thread, then what it did there.
static RELAYOUT_PENDING: AtomicBool = AtomicBool::new(false);
static RELAYOUT_DONE: AtomicBool = AtomicBool::new(false);
/// `IUIManager::OnResolutionChange`, resolved for the game's thread.
static RELAYOUT_CALL: AtomicUsize = AtomicUsize::new(0);
static RELAYOUT_RESULT: Mutex<String> = Mutex::new(String::new());

/// The game does not tell its levels' UI when the video settings change size (`engine.rs`), so
/// the UI keeps the projection of the size it was made at: the pause menu then sits shrunk in a
/// corner, in VR and on the monitor after it. Once the swapchain is `size` (waiting up to `wait`),
/// every level's UI manager makes its projection again from the size now in the video settings,
/// as the game's own handler would (not called: it crashes the game, `engine.rs`), on the game's
/// thread at its next
/// frame ([`engine::UI_ON_RESOLUTION_CHANGE`]). While VR runs the UI keeps the monitor's size
/// (`hud::ui_size`), so this only makes sure each camera is of that size.
fn relayout(gamedll: &Module, chain: &IDXGISwapChain, size: (u32, u32), wait: Duration, place_again: bool) {
    let started = Instant::now();
    while size_of(chain) != Some(size) {
        if started.elapsed() >= wait {
            log!("menus not laid out again: the swapchain is not {}x{} yet (the game applies it while it runs)", size.0, size.1);
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let Ok(engine_module) = monaka_producer::require_build(engine::ENGINE, engine::ENGINE_SHA256) else { return };
    let Some(on_change) = engine_module.export(engine::UI_ON_RESOLUTION_CHANGE) else {
        log!("menus not laid out again: the engine does not export IUIManager::OnResolutionChange");
        return;
    };
    let mut hooks = Hooks::default();
    // SAFETY: the export's type is `float IGame::GetGameTimeDelta() const`.
    if let Err(e) = unsafe { eng_chr::game::hook(&mut hooks, gamedll) }.map_err(|e| e.to_string()).and_then(|_| hooks.enable().map_err(|e| e.to_string())) {
        log!("menus not laid out again: {e}");
        return;
    }
    RELAYOUT_CALL.store(on_change, Relaxed);
    RELAYOUT_DONE.store(false, Relaxed);
    PLACE_AGAIN.store(place_again, Relaxed);
    RELAYOUT_PENDING.store(true, Release);
    eng_chr::game::observe(relayout_on_game_thread);
    let started = Instant::now();
    while !RELAYOUT_DONE.load(Acquire) && started.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(10));
    }
    RELAYOUT_PENDING.store(false, Release);
    if let Err(e) = hooks.disable() {
        log!("menus laid out again: unhooking failed: {e}");
    }
    if RELAYOUT_DONE.load(Acquire) {
        log!("menus laid out again for {}x{}: {}", size.0, size.1, RELAYOUT_RESULT.lock().map(|r| r.clone()).unwrap_or_default());
    } else {
        log!("menus not laid out again: the game ran no frame in 2 s (paused?)");
    }
}

/// On the game's thread, once: each level's UI manager makes its projection again.
fn relayout_on_game_thread(game: usize) {
    if !RELAYOUT_PENDING.swap(false, AcqRel) {
        return;
    }
    // SAFETY: the engine's export, with this type (`engine.rs`).
    let on_change = unsafe { std::mem::transmute::<usize, engine::UiOnResolutionChangeFn>(RELAYOUT_CALL.load(Relaxed)) };
    let levels = mem::read::<usize>(game + 8).and_then(|g| Some((mem::read::<usize>(g + engine::GAME_LEVELS)?, mem::read::<i32>(g + engine::GAME_LEVEL_COUNT)?)));
    let result = match levels.filter(|&(a, c)| a != 0 && (0..=64).contains(&c)) {
        Some((array, count)) => {
            let mut notes = Vec::new();
            let mut done = 0;
            for i in 0..count as usize {
                let Some(level) = mem::read::<usize>(array + 8 * i).filter(|&l| l != 0) else {
                    notes.push("none".to_owned());
                    continue;
                };
                // The array holds the levels themselves (`CLevel`), what an `ILevel` keeps at +8:
                // the UI manager is `[[level + 0x568] + 0x1770]`, each step checked, and the
                // manager's own first field, which the change reads.
                let holder = mem::read::<usize>(level + engine::LEVEL_UI_HOLDER).filter(|&p| p != 0);
                let manager = holder.and_then(|h| mem::read::<usize>(h + engine::UI_HOLDER_MANAGER)).filter(|&m| m != 0);
                let inner = manager.and_then(mem::read::<usize>).filter(|&p| p != 0);
                match (holder, manager, inner) {
                    (Some(holder), Some(manager), Some(_)) => {
                        if PLACE_AGAIN.load(Relaxed) {
                            let layout = holder + engine::UI_HOLDER_AUTO_LAYOUT;
                            if let Some(flags) = mem::read::<u8>(layout) {
                                mem::write(layout, flags | engine::AUTO_LAYOUT_DIRTY);
                            }
                        }
                        // SAFETY: the engine's own call on its live level's UI manager, on the
                        // game's thread, as the game's handler makes it (at the fixed UI's size
                        // while VR runs, `hud::ui_size`).
                        crate::hud::ui_size::as_fixed(|| unsafe { on_change(manager as *mut core::ffi::c_void) });
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
        None => "the level list was unreadable".into(),
    };
    *RELAYOUT_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = result;
    RELAYOUT_DONE.store(true, Release);
}

/// [`restore`], then the levels' UI laid out again for that size (the stop).
pub fn restore_and_relayout() {
    let size = screen_before();
    restore();
    let Some(size) = size else { return };
    let Ok(gamedll) = monaka_producer::require_build(engine::GAMEDLL, engine::GAMEDLL_SHA256) else { return };
    let Ok(renderer) = monaka_producer::require_build(engine::RENDERER, engine::RENDERER_SHA256) else { return };
    let Ok(chain) = crate::game_swapchain(&renderer) else { return };
    relayout(&gamedll, &chain, size, Duration::from_millis(1500), true);
}

/// Asks the game for the mode it had before [`switch_to`], if that changed it.
pub fn restore() {
    crate::hud::ui_size::end();
    BEFORE.store(0, Relaxed);
    if let Some((settings, original)) = SWITCHED.lock().unwrap_or_else(|e| e.into_inner()).take() {
        request(settings, &original);
        let width = u32::from_le_bytes(original[0..4].try_into().expect("4 bytes"));
        let height = u32::from_le_bytes(original[4..8].try_into().expect("4 bytes"));
        log!("requested the original {width}x{height} back");
    }
}
