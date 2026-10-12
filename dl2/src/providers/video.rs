//! Provider: video (`monaka_stereo::eye_size::Resizes`), asking for a render size and putting the
//! game's back (its component is `crate::EYE_SIZE`).
//!
//! Rendering at the headset's eye size while VR runs (`monaka_stereo::eye_size`), windowed, through
//! the path the options menu uses, for either renderer; the original is asked back at stop.
//!
//! `CVideoSettings` sits at `[game + VIDEO_SETTINGS_IN_GAME]` or `[[game + 8] + ...]` (the one whose
//! mode and size match what is presented); its +8 is the settings struct (+4 window mode, +8/+0xc
//! resolution, +0xb8 change mask: 1 window mode, 2 resolution); its +0x699/+0x69b request an apply on
//! the game's next frame.

use crate::Dl2;
use crate::providers::{engine, game};
use monaka_core::sync::Locked;
use monaka_hook::mem;
use monaka_producer::log;
use monaka_stereo::eye_size::{Ask, Resizes};
use std::sync::Mutex;

/// What the switch changed: the settings, and the original mode and size.
struct Switched {
    settings: usize,
    mode: u32,
    size: (u32, u32),
}

static SWITCHED: Mutex<Option<Switched>> = Mutex::new(None);

fn find_video_settings(game_object: usize, (width, height): (u32, u32)) -> Option<usize> {
    let candidates = [mem::read::<usize>(game_object + engine::VIDEO_SETTINGS_IN_GAME), mem::read::<usize>(game_object + 8).and_then(|g| mem::read::<usize>(g + engine::VIDEO_SETTINGS_IN_GAME))];
    candidates.into_iter().flatten().find(|&c| {
        c != 0 && mem::read::<u32>(c + 0xc).is_some_and(|mode| mode <= 2) && mem::read::<u32>(c + 0x10) == Some(width) && mem::read::<u32>(c + 0x14) == Some(height)
    })
}

fn request_video_mode(settings: usize, mode: u32, (width, height): (u32, u32)) {
    let s = settings + 8;
    mem::write(s + 4, mode);
    mem::write(s + 8, width);
    mem::write(s + 0xc, height);
    let mask = mem::read::<u64>(s + 0xb8).unwrap_or(0);
    mem::write(s + 0xb8, mask | 3);
    mem::write(settings + 0x699, 1u8);
    mem::write(settings + 0x69b, 1u8);
}

/// Whether a frame of `width` x `height` may be published (the channel made at its size).
pub fn at_headset_size(width: u32, height: u32) -> bool {
    monaka_stereo::eye_size::ready::<Dl2>((width, height))
}

/// Asks for `eye` (width, height), windowed, while the game presents at `presented`.
pub fn ask(eye: (u32, u32), presented: (u32, u32)) -> Ask {
    let Some((game_object, _)) = game::get().counter() else { return Ask::NotYet };
    let Some(settings) = find_video_settings(game_object, presented) else { return Ask::Cannot("video settings not found".into()) };
    let mode = mem::read::<u32>(settings + 0xc).unwrap_or(0);
    *SWITCHED.locked() = Some(Switched { settings, mode, size: presented });
    request_video_mode(settings, 0, eye);
    Ask::Asked
}

/// Asks for the original mode and size back, if they were changed.
pub fn restore() {
    if let Some(s) = SWITCHED.locked().take() {
        request_video_mode(s.settings, s.mode, s.size);
        log!("asking for the original {}x{} (mode {}) back", s.size.0, s.size.1, s.mode);
    }
}

impl Resizes for Dl2 {
    fn ask(eye: (u32, u32), presented: (u32, u32)) -> Ask {
        ask(eye, presented)
    }

    fn put_back() {
        restore();
    }
}
