//! Head aim's traces (`crate::player::aim`): with `debug_flags` bit `AIM_LOG`, each steering update of
//! head aim (the first few, then every 120th) and the head yaw baked into the first levelled views.

use super::options;
use eng_chr::headaim::Steered;
use monaka_producer::log;

/// A levelled game camera `view` of frame `counter`, before the baked head yaw is turned out.
pub fn view(view: &[f32; 12], counter: u32) {
    let aim = &crate::player::aim::AIM;
    if options().aim_log && aim.live() {
        monaka_producer::log_first!(6, "view (counter {counter}): game camera yaw {:.2}, baked head yaw {:.2}", view[2].atan2(view[10]).to_degrees(), aim.baked_at(Some(counter as u64)).to_degrees());
    }
}

/// Head aim steered the character at the player camera update of `counter`; `camera_pitch` is
/// the camera's own (radians).
pub fn steered(steered: &Steered, counter: Option<u64>, camera_pitch: f32) {
    if options().aim_log && (steered.update < 6 || steered.update.is_multiple_of(120)) {
        log!(
            "head aim {} (counter {counter:?}): camera pitch {:.2} (baked head yaw {:.2}); look written yaw {:.2} pitch {:.2}; own yaw {:.2} pitch {:.2}",
            steered.update,
            camera_pitch.to_degrees(),
            steered.baked.to_degrees(),
            steered.written.yaw,
            steered.written.pitch,
            steered.own.yaw,
            steered.own.pitch
        );
    }
}
