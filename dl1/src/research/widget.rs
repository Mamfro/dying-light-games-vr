//! `probe_widget=<name>` (with the in-world HUD): the named widget of the game's UI tree
//! (`crate::hud::ui`), its first part (kind and corner, layout pixels) at every snapshot, with the eye
//! being chosen, the present, and which camera the player camera held as the game laid it out.
//! Kept in memory and logged in one go once [`WATCHED_LINES`] are in: logged a line at a time,
//! the game thread stuttered and the eyes got each other's images (2026-10-07).

use super::options;
use crate::hud::ui::Leaf;
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::*};

static WATCHED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static WATCH_DONE: AtomicBool = AtomicBool::new(false);
const WATCHED_LINES: usize = 600;

/// The widget to watch (read at every snapshot), if any.
pub fn watched() -> Option<&'static str> {
    options().widget.as_deref()
}

/// The watched widget `name` read at a snapshot, with its drawn `leaves`.
pub fn note(name: &str, leaves: &[Leaf]) {
    if WATCH_DONE.load(Relaxed) {
        return;
    }
    let Some(first) = leaves.first() else { return };
    let camera = crate::view::stereo::live_camera_look().map_or("none".to_owned(), |(yaw, pitch, turned)| format!("{yaw:.2} {pitch:.2}{}", if turned { " view" } else { "" }));
    let line = format!(
        "{} eye {} present {:?} camera {camera}: {}({:.1} {:.1})",
        monaka_channel::tick(),
        crate::view::stereo::eye(),
        crate::view::stereo::drawing_present(),
        if first.text { "text" } else { "image" },
        first.at[0],
        first.at[1]
    );
    let full = WATCHED.lock().is_ok_and(|mut lines| {
        lines.push(line);
        lines.len() >= WATCHED_LINES
    });
    if full {
        report_watched(name);
    }
}

/// Logs what the probe gathered, once: when it has [`WATCHED_LINES`], or at the stop with what it
/// has.
fn report_watched(name: &str) {
    if WATCH_DONE.swap(true, Relaxed) {
        return;
    }
    let Ok(lines) = WATCHED.lock() else { return };
    log!("probe widget {name}: {} snapshots while drawn (tick ms, eye, present, the player camera's yaw and pitch (view: a turn the view wrote), its first part):\n{}", lines.len(), lines.join("\n"));
}

/// The end of a run.
pub fn report() {
    if let Some(name) = watched() {
        report_watched(name);
    }
}
