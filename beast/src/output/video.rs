//! Rendering at the headset's eye size while VR runs (`eye_size=WxH`), as DL1 and DL2 do: at the
//! start the game's own video settings are asked for that size, windowed, through the renderer's
//! exported setters and the apply request the options menu uses. The game applies it on its own
//! frames, so not while paused (it pauses without focus); until it presents at that size, frames are
//! not published ([`ready`]), since the stereo channel is made at the first published frame's size.
//! The stop asks for the original mode and size back the same way. Layout in
//! [`engine`](crate::engine) (`VIDEO_*`).

use crate::engine::{self, SetResolutionFn, SetWindowModeFn};
use monaka_hook::module::Module;
use eng_chr::game;
use monaka_hook::mem;
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// The presented size (width << 32 | height).
static PRESENTED: AtomicU64 = AtomicU64::new(0);
/// What the switch changed: the `SVideoSettings`, the setters, and the original mode and size.
struct Switched {
    settings: usize,
    set_resolution: SetResolutionFn,
    set_window_mode: SetWindowModeFn,
    mode: u32,
    size: (u32, u32),
    bit_depth: u32,
}

static SWITCHED: Mutex<Option<Switched>> = Mutex::new(None);

/// The size the game just presented (from the present request).
pub fn presented(width: u32, height: u32) {
    PRESENTED.store(((width as u64) << 32) | height as u64, Relaxed);
}

fn presented_size() -> Option<(u32, u32)> {
    let packed = PRESENTED.load(Relaxed);
    (packed != 0).then_some(((packed >> 32) as u32, packed as u32))
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < limit {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

fn request(settings: usize, set_resolution: SetResolutionFn, set_window_mode: SetWindowModeFn, mode: u32, (width, height): (u32, u32), bit_depth: u32) {
    // SAFETY: the renderer's own setters on the game's live settings (they only store the values
    // and mark them changed, as the options menu does).
    unsafe {
        set_window_mode(settings, mode);
        set_resolution(settings, width, height, bit_depth);
    }
    let owner = settings - engine::VIDEO_SETTINGS_STRUCT;
    for request in engine::VIDEO_APPLY_REQUESTS {
        mem::write(owner + request, 1u8);
    }
}

/// The size asked for (width << 32 | height; 0: none pending), and presents seen at another size
/// since: the game applies the switch on its own frames, so a paused game simply keeps it pending,
/// while one that keeps presenting at its old size has refused it.
static PENDING: AtomicU64 = AtomicU64::new(0);
static PRESENTS_AT_OLD_SIZE: AtomicU64 = AtomicU64::new(0);
/// Presents at the old size after which the switch counts as refused (well under a second of
/// normal play; minutes of a paused game's few frames).
const REFUSED_AFTER: u64 = 90;

/// Whether frames may be published now: no switch pending, or the game presents at the size asked
/// for. A refused switch is undone here (the original asked back) and publishing goes ahead.
pub fn ready() -> bool {
    let pending = PENDING.load(Relaxed);
    if pending == 0 {
        return true;
    }
    let target = ((pending >> 32) as u32, pending as u32);
    match presented_size() {
        Some(size) if size == target => {
            PENDING.store(0, Relaxed);
            log!("presenting at {}x{}", target.0, target.1);
            true
        }
        Some(_) if PRESENTS_AT_OLD_SIZE.fetch_add(1, Relaxed) + 1 >= REFUSED_AFTER => {
            PENDING.store(0, Relaxed);
            log!("the game kept presenting at its own size after {REFUSED_AFTER} frames; asking for the original back");
            restore();
            true
        }
        _ => false,
    }
}

/// Asks for `eye` (width, height), windowed; [`ready`] then holds publishing until the game presents
/// at that size. Errors when the request cannot be made (the game carries on at its size).
pub fn switch_to(renderer: &Module, eye: (u32, u32)) -> Result<(), String> {
    if !wait_for(Duration::from_secs(3), || game::game().is_some() && presented_size().is_some()) {
        return Err("the game object or its presented size was not seen within 3 s".into());
    }
    let presented = presented_size().expect("seen above");
    if presented == eye {
        log!("already presenting at the headset eye size {}x{}", eye.0, eye.1);
        return Ok(());
    }
    let game = game::game().ok_or("no game object")?;
    let owner = game::video_settings(game, engine::VIDEO_SETTINGS_IN_GAME).ok_or("no video settings")?;
    let settings = owner + engine::VIDEO_SETTINGS_STRUCT;
    let read = |offset: usize| mem::read::<u32>(settings + offset);
    let (Some(mode), Some(width), Some(height), Some(bit_depth)) =
        (read(engine::VIDEO_WINDOW_MODE), read(engine::VIDEO_WIDTH), read(engine::VIDEO_HEIGHT), read(engine::VIDEO_BIT_DEPTH))
    else {
        return Err("video settings unreadable".into());
    };
    // The settings must describe what the game presents, or this is not the object it is believed.
    if mode > 2 || (width, height) != presented {
        return Err(format!("video settings say mode {mode} {width}x{height}, but the game presents {}x{}", presented.0, presented.1));
    }
    let export = |name: &str| renderer.export(name).ok_or_else(|| format!("{name} is not exported"));
    // SAFETY: the renderer's exported setters have these signatures (their mangled names say so).
    let (set_resolution, set_window_mode): (SetResolutionFn, SetWindowModeFn) =
        unsafe { (std::mem::transmute::<usize, SetResolutionFn>(export(engine::SET_RESOLUTION)?), std::mem::transmute::<usize, SetWindowModeFn>(export(engine::SET_WINDOW_MODE)?)) };
    log!("asking for the headset eye size {}x{} windowed (was {width}x{height}, mode {mode}); publishing waits for it (the game applies it while it has focus)", eye.0, eye.1);
    *SWITCHED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Switched { settings, set_resolution, set_window_mode, mode, size: (width, height), bit_depth });
    PRESENTS_AT_OLD_SIZE.store(0, Relaxed);
    PENDING.store(((eye.0 as u64) << 32) | eye.1 as u64, Relaxed);
    request(settings, set_resolution, set_window_mode, engine::WINDOWED, eye, bit_depth);
    Ok(())
}

/// Asks for the original mode and size back (applied on the game's next frame).
pub fn restore() {
    PENDING.store(0, Relaxed);
    if let Some(s) = SWITCHED.lock().unwrap_or_else(|e| e.into_inner()).take() {
        log!("asking for the original {}x{} (mode {}) back", s.size.0, s.size.1, s.mode);
        request(s.settings, s.set_resolution, s.set_window_mode, s.mode, s.size, s.bit_depth);
    }
}
