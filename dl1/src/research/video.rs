//! `probe_video`: where the game keeps the current resolution (found the video settings layout of
//! `crate::output::video`): on the game's frames ([`eng_chr::game::observe`]), the objects around the game
//! object searched for the back buffer's size.

use crate::engine;
use monaka_hook::mem;
use monaka_hook::probe::{class_name, find_u32_pair};
use monaka_producer::log;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::*};

static SIZE: AtomicU64 = AtomicU64::new(0);
static PROBES: AtomicU32 = AtomicU32::new(0);

/// The current back buffer size, for the probe to look for.
pub fn set_size(width: u32, height: u32) {
    SIZE.store(((width as u64) << 32) | height as u64, Relaxed);
}

/// On the game's frames: where the size is kept, twice.
pub fn probe_frame(game: usize) {
    if crate::view::stereo::capturing() {
        let n = PROBES.fetch_add(1, Relaxed);
        if n == 100 || n == 5000 {
            probe(game);
        }
    }
}

fn describe(label: &str, object: usize, length: usize, width: u32, height: u32) {
    let pairs = find_u32_pair(object, length, width, height);
    let shown: Vec<String> = pairs.iter().map(|o| format!("+{o:#x}")).collect();
    log!(
        "video probe: {label} {object:#x} {}: {width}x{height} at {}",
        class_name(object).unwrap_or_else(|| "(no RTTI)".into()),
        if shown.is_empty() { "none".into() } else { shown.join(" ") }
    );
}

fn probe(game: usize) {
    let size = SIZE.load(Relaxed);
    let (width, height) = ((size >> 32) as u32, size as u32);
    describe("game", game, 0x400, width, height);
    let Some(inner) = mem::read::<usize>(game + 8) else { return };
    describe("game+8", inner, 0x2000, width, height);
    let Some(settings) = eng_chr::game::video_settings(game, engine::VIDEO_SETTINGS_IN_GAME) else { return };
    describe("settings", settings, 0x1000, width, height);
    let mut bytes = [0u8; 0xc0];
    if mem::read_bytes(settings, &mut bytes) {
        for (row, chunk) in bytes.chunks(16).enumerate() {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            log!("video probe: settings+{:#04x}: {}", row * 16, hex.join(" "));
        }
    }
}
