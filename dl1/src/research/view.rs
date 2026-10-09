//! The eye cameras' probes, at each view setup (`crate::view::stereo`): `probe_frames`,
//! `probe_cutscene` and `probe_rig` (flicker hunting).

use super::options;
use crate::view::stereo::drawing_present;
use monaka_core::camera::{self, Mat34, apply_head, distance, rigid_inverse};
use monaka_core::protocol::HeadPose;
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};

/// `probe_frames`: one line per view setup for 1500 calls after the first 120, to see what changes
/// between frames (the view and its render state, whether the cache was stale, whether head aim
/// steered, the game camera's position and backward axis).
pub fn frame(eye: usize, view: usize, state: usize, stale: bool, inverse: &Mat34) {
    if !options().frames {
        return;
    }
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let call = CALLS.fetch_add(1, Relaxed);
    if !(120..1620).contains(&call) {
        return;
    }
    let (stored, wrote) = super::look::last_yaws();
    log!(
        "frame {:?} eye {eye} view {view:#x} state {state:#x} stale {stale} aim live {} updates {} baked {:.2} stored {stored:.2} wrote {wrote:.2} pos [{:.3} {:.3} {:.3}] back [{:.3} {:.3} {:.3}]",
        drawing_present(),
        crate::player::aim::live(),
        crate::player::aim::updates(),
        crate::player::aim::baked_yaw().to_degrees(),
        inverse[3],
        inverse[7],
        inverse[11],
        inverse[2],
        inverse[6],
        inverse[10]
    );
}

/// `probe_cutscene`: lines left to log once the trace has begun, and whether it has: it begins when
/// the game's camera first moves, so paused in a cutscene and then let go, it covers the cutscene.
static CUTSCENE_LINES: AtomicU64 = AtomicU64::new(3000);
static CUTSCENE_TRACING: AtomicBool = AtomicBool::new(false);
static CUTSCENE_STILL: Mutex<Option<Mat34>> = Mutex::new(None);

/// Whether the cutscene trace may log one more line (and counts it).
pub fn cutscene_line() -> bool {
    CUTSCENE_TRACING.load(Relaxed) && CUTSCENE_LINES.fetch_update(Relaxed, Relaxed, |n| n.checked_sub(1)).is_ok()
}

/// `probe_cutscene`: one eye camera: the view cache as read, the base it was made from with its
/// baked yaw, and the centre made.
#[allow(clippy::too_many_arguments)]
pub fn cutscene(eye: usize, chosen_at: u64, turned: bool, stale: bool, cache: &Mat34, base: &Mat34, baked: f32, center: &Mat34) {
    if !options().cutscene {
        return;
    }
    if !CUTSCENE_TRACING.load(Relaxed) {
        let Ok(mut still) = CUTSCENE_STILL.lock() else { return };
        let moved = still.is_some_and(|s| distance(&s, base) > 0.002);
        *still = Some(*base);
        if !moved {
            return;
        }
        CUTSCENE_TRACING.store(true, Relaxed);
        log!("cutscene trace: the game's camera moved; tracing");
    }
    if !cutscene_line() {
        return;
    }
    // Camera-to-world: column 2 is the back axis, column 3 the position.
    let yaw = |m: &Mat34| m[2].atan2(m[10]).to_degrees();
    let pitch = |m: &Mat34| (-m[6]).clamp(-1.0, 1.0).asin().to_degrees();
    log!(
        "cs view p{chosen_at} eye {eye} {} stale {stale} cutscene {}: cache [{:.3} {:.3} {:.3}] yaw {:.2} pitch {:.2}; base [{:.3} {:.3} {:.3}] yaw {:.2} pitch {:.2} baked {:.2} -> centre [{:.3} {:.3} {:.3}] yaw {:.2} pitch {:.2}",
        if turned { "turned" } else { "rebuilt" },
        crate::player::aim::cutscene(),
        cache[3],
        cache[7],
        cache[11],
        yaw(cache),
        pitch(cache),
        base[3],
        base[7],
        base[11],
        yaw(base),
        pitch(base),
        baked.to_degrees(),
        center[3],
        center[7],
        center[11],
        yaw(center),
        pitch(center)
    );
}

/// `probe_rig`: for 600 eye frames after the first 300, the latest hand-rig sample against this
/// eye: the palm's position in the eye's space, and how far the rig's tracking origin is from the
/// view's (position mm, yaw degrees). Both should hold steady with a still head and hand.
pub fn rig(eye: usize, center: &Mat34, desired: &Mat34, pose: &HeadPose, turned: bool) {
    if !options().rig {
        return;
    }
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let call = CALLS.fetch_add(1, Relaxed);
    if !(300..900).contains(&call) {
        return;
    }
    let Some(rig) = super::arms::last_rig() else { return };
    let head = apply_head(&camera::translation([0.0; 3]), pose.orientation, pose.position);
    let origin = camera::compose(center, &rigid_inverse(&head));
    let palm = camera::compose(&rigid_inverse(desired), &rig.palm);
    let d = [origin[3] - rig.origin[3], origin[7] - rig.origin[7], origin[11] - rig.origin[11]];
    let yaw = |m: &Mat34| m[2].atan2(m[10]).to_degrees();
    let game_moved = [rig.game[3] - origin[3], rig.game[7] - origin[7], rig.game[11] - origin[11]];
    log!(
        "rig {:?} eye {eye} turned {turned} rig call {} at present {}: palm in eye [{:.3} {:.3} {:.3}] origin diff [{:.1} {:.1} {:.1}] mm yaw {:.2}; rig camera from view origin [{:.3} {:.3} {:.3}]",
        drawing_present(),
        rig.call,
        rig.present,
        palm[3],
        palm[7],
        palm[11],
        d[0] * 1000.0,
        d[1] * 1000.0,
        d[2] * 1000.0,
        yaw(&origin) - yaw(&rig.origin),
        game_moved[0],
        game_moved[1],
        game_moved[2]
    );
}
