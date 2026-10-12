//! Provider: video (`monaka_stereo::eye_size::Resizes`), asking for a render size and putting the
//! game's back, with the eye size's component ([`EYE_SIZE`]).
//!
//! Rendering at the headset's eye size while VR runs (`monaka_stereo::eye_size`): the game's own
//! video settings asked for that size, windowed, through the renderer's exported setters and the
//! apply request the options menu uses; the stop asks for the original mode and size back the same
//! way. Layout in [`engine`](crate::providers::engine) (`VIDEO_*`).

use crate::providers::engine::{self, SetResolutionFn, SetWindowModeFn};
use crate::Beast;
use eng_chr::feature::gamedll;
use eng_chr::providers::game;
use monaka_core::sync::Locked;
use monaka_hook::mem;
use monaka_hook::module::Module;
use monaka_producer::game::{Lack, Start};
use monaka_producer::log;
use monaka_stereo::eye_size::{self, Ask, Resizes};
use std::sync::Mutex;

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
/// The renderer whose setters make the switch.
static RENDERER: Mutex<Option<Module>> = Mutex::new(None);

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

/// The switch is made through `renderer`'s setters.
pub fn set_renderer(renderer: Module) {
    *RENDERER.locked() = Some(renderer);
}

/// Asks for `eye` (width, height), windowed, while the game presents at `presented`.
pub fn ask(eye: (u32, u32), presented: (u32, u32)) -> Ask {
    let Some(game) = game::game() else { return Ask::NotYet };
    let Some(renderer) = *RENDERER.locked() else { return Ask::Cannot("no renderer".into()) };
    let Some(owner) = game::video_settings(game, engine::VIDEO_SETTINGS_IN_GAME) else { return Ask::Cannot("no video settings".into()) };
    let settings = owner + engine::VIDEO_SETTINGS_STRUCT;
    let read = |offset: usize| mem::read::<u32>(settings + offset);
    let (Some(mode), Some(width), Some(height), Some(bit_depth)) =
        (read(engine::VIDEO_WINDOW_MODE), read(engine::VIDEO_WIDTH), read(engine::VIDEO_HEIGHT), read(engine::VIDEO_BIT_DEPTH))
    else {
        return Ask::Cannot("video settings unreadable".into());
    };
    // The settings must describe what the game presents, or this is not the object it is believed.
    if mode > 2 || (width, height) != presented {
        return Ask::Cannot(format!("video settings say mode {mode} {width}x{height}, but the game presents {}x{}", presented.0, presented.1));
    }
    let (Some(set_resolution), Some(set_window_mode)) = (renderer.export(engine::SET_RESOLUTION), renderer.export(engine::SET_WINDOW_MODE)) else {
        return Ask::Cannot("the renderer does not export its video setters".into());
    };
    // SAFETY: the renderer's exported setters have these signatures (their mangled names say so).
    let (set_resolution, set_window_mode) =
        unsafe { (std::mem::transmute::<usize, SetResolutionFn>(set_resolution), std::mem::transmute::<usize, SetWindowModeFn>(set_window_mode)) };
    *SWITCHED.locked() = Some(Switched { settings, set_resolution, set_window_mode, mode, size: (width, height), bit_depth });
    request(settings, set_resolution, set_window_mode, engine::WINDOWED, eye, bit_depth);
    Ask::Asked
}

/// Asks for the original mode and size back (applied on the game's next frame).
pub fn restore() {
    if let Some(s) = SWITCHED.locked().take() {
        log!("asking for the original {}x{} (mode {}) back", s.size.0, s.size.1, s.mode);
        request(s.settings, s.set_resolution, s.set_window_mode, s.mode, s.size, s.bit_depth);
    }
}

/// The eye size component's install (`crate::EYE_SIZE`): the game's frame time hooked, the size
/// asked for at the presents once the hooks are in ([`ask`]).
pub(crate) fn install_eye_size(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    // SAFETY: the fingerprinted game DLL imports GetGameTimeDelta with this type.
    unsafe { eng_chr::providers::game::hook(start.hooks, gamedll(start)?)? };
    set_renderer(start.build.renderer);
    eye_size::want(start)
}

impl Resizes for Beast {
    fn ask(eye: (u32, u32), presented: (u32, u32)) -> Ask {
        ask(eye, presented)
    }

    fn put_back() {
        restore();
    }
}
