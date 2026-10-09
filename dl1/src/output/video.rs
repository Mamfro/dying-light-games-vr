//! Rendering at the headset's eye size while VR runs, as DL2 does: the start asks the game's own
//! video settings for `eye_size` and waits for the swapchain to follow, before anything sized
//! from the back buffer exists; the stop asks for the original size back. Layout in
//! [`engine`](crate::engine) (`VIDEO_*`), found with `probe_video` (`research::video`).

use crate::engine;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, mem};
use monaka_producer::log;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;

/// The settings object and the requested mode it had before the switch.
static SWITCHED: Mutex<Option<(usize, [u8; engine::VIDEO_MODE_SIZE])>> = Mutex::new(None);

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
    if current == (width, height) {
        return Ok(());
    }
    let mut hooks = Hooks::default();
    // SAFETY: the export's type is `float IGame::GetGameTimeDelta() const`.
    unsafe { eng_chr::game::hook(&mut hooks, gamedll) }.map_err(|e| e.to_string())?;
    hooks.enable().map_err(|e| e.to_string())?;
    let game = eng_chr::game::wait_for_game(Duration::from_secs(2));
    hooks.disable().map_err(|e| e.to_string())?;
    let game = game.ok_or("the game made no frame in two seconds (paused, or in a loading screen?)")?;
    let settings = settings_of(game).ok_or("no video settings object")?;
    let applied = (mem::read::<u32>(settings + engine::VIDEO_APPLIED), mem::read::<u32>(settings + engine::VIDEO_APPLIED + 4));
    // A run that never asked for the size back (the game crashed or was killed) leaves the eye size
    // in the settings, and the game saves it: started with it, a borderless window that tall is
    // clamped to the screen (2644x2044 on a 2160 one) and the square view is drawn squashed into it
    // (2026-10-06). Asked again, the game applies it as it does in play; the size handed back at the
    // end is then the screen's.
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
    let mut mode = original;
    mode[0..4].copy_from_slice(&width.to_le_bytes());
    mode[4..8].copy_from_slice(&height.to_le_bytes());
    request(settings, &mode);
    *SWITCHED.lock().unwrap_or_else(|e| e.into_inner()) = Some((settings, original));
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
            restore();
            return Err(format!("the swapchain is still {now:?} after {} ms; size request withdrawn", wait.as_millis()));
        }
    }
}

/// Asks the game for the mode it had before [`switch_to`], if that changed it.
pub fn restore() {
    if let Some((settings, original)) = SWITCHED.lock().unwrap_or_else(|e| e.into_inner()).take() {
        request(settings, &original);
        let width = u32::from_le_bytes(original[0..4].try_into().expect("4 bytes"));
        let height = u32::from_le_bytes(original[4..8].try_into().expect("4 bytes"));
        log!("requested the original {width}x{height} back");
    }
}
