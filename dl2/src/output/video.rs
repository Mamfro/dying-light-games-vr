//! Rendering at the headset's eye size while VR runs (`eye_size=WxH`), windowed, through the path the
//! options menu uses, for either renderer; the original is asked back at stop. Until the game
//! presents at that size the stereo channel is not made (it takes the first published frame's size),
//! for at most ten seconds; the game applies the change on its own frames, so only while it has
//! focus.
//!
//! `CVideoSettings` sits at `[game + VIDEO_SETTINGS_IN_GAME]` or `[[game + 8] + ...]` (the one whose
//! mode and size match what is presented); its +8 is the settings struct (+4 window mode, +8/+0xc
//! resolution, +0xb8 change mask: 1 window mode, 2 resolution); its +0x699/+0x69b request an apply on
//! the game's next frame.

use crate::config;
use crate::{engine, game};
use monaka_hook::mem;
use monaka_producer::log;
use std::sync::Mutex;

struct Video {
    settings: usize,
    original_mode: u32,
    original_size: (u32, u32),
    changed: bool,
    requested_at: u64,
    gave_up: bool,
}

static VIDEO: Mutex<Video> = Mutex::new(Video { settings: 0, original_mode: 0, original_size: (0, 0), changed: false, requested_at: 0, gave_up: false });

fn find_video_settings(width: u32, height: u32) -> Option<usize> {
    let (game_object, _) = game::get().counter()?;
    let candidates = [mem::read::<usize>(game_object + engine::VIDEO_SETTINGS_IN_GAME), mem::read::<usize>(game_object + 8).and_then(|g| mem::read::<usize>(g + engine::VIDEO_SETTINGS_IN_GAME))];
    candidates.into_iter().flatten().find(|&c| {
        c != 0 && mem::read::<u32>(c + 0xc).is_some_and(|mode| mode <= 2) && mem::read::<u32>(c + 0x10) == Some(width) && mem::read::<u32>(c + 0x14) == Some(height)
    })
}

fn request_video_mode(settings: usize, mode: u32, width: u32, height: u32) {
    let s = settings + 8;
    mem::write(s + 4, mode);
    mem::write(s + 8, width);
    mem::write(s + 0xc, height);
    let mask = mem::read::<u64>(s + 0xb8).unwrap_or(0);
    mem::write(s + 0xb8, mask | 3);
    mem::write(settings + 0x699, 1u8);
    mem::write(settings + 0x69b, 1u8);
}

/// Whether a frame of `width` x `height` may be published (the channel made at its size): the
/// headset size is not asked for, or the game presents at it, or it could not be had. Asks for it the
/// first time.
pub fn at_headset_size(width: u32, height: u32) -> bool {
    let Some((eye_width, eye_height)) = config::get().eye_size else { return true };
    let mut video = VIDEO.lock().unwrap_or_else(|e| e.into_inner());
    if video.gave_up || (width, height) == (eye_width, eye_height) {
        return true;
    }
    if !video.changed {
        if video.requested_at != 0 {
            // Could not be applied; carry on at the game's size.
            return true;
        }
        video.requested_at = game::tick();
        let Some(settings) = find_video_settings(width, height) else {
            log!("video settings not found; rendering at the game's {width}x{height}");
            return true;
        };
        video.settings = settings;
        video.original_mode = mem::read::<u32>(settings + 0xc).unwrap_or(0);
        video.original_size = (width, height);
        request_video_mode(settings, 0, eye_width, eye_height);
        video.changed = true;
        log!("requested the headset eye size {eye_width}x{eye_height} windowed (was {width}x{height} mode {}); it applies while the game has focus", video.original_mode);
        return false;
    }
    if game::tick() - video.requested_at < 10_000 {
        return false;
    }
    log!("the game is still at {width}x{height} ten seconds after the headset size was asked for; carrying on");
    video.gave_up = true;
    true
}

/// Asks for the original mode and size back, if they were changed.
pub fn restore() {
    let mut video = VIDEO.lock().unwrap_or_else(|e| e.into_inner());
    if video.changed {
        video.changed = false;
        request_video_mode(video.settings, video.original_mode, video.original_size.0, video.original_size.1);
        log!("restored {}x{} mode {}", video.original_size.0, video.original_size.1, video.original_mode);
    }
}
