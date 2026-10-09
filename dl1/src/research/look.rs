//! Head aim's probes (`crate::player::aim`).
//!
//! - `probe_look` found DL1's look fields (2026-10-05): it logs who calls `FromForwardUpPos` on
//!   the player camera, the classes of the objects around that camera (and of the player objects
//!   those point to), and which of their floats equal the camera's pitch (look well up or down
//!   with the mouse before attaching); with head aim on it also traces the steering.
//! - `probe_cutscene`: each look update while the cutscene trace runs ([`super::view`]).

use super::options;
use monaka_hook::mem;
use monaka_hook::module::{self, Module};
use monaka_hook::probe::{angle_matcher, class_name, complete_object, find_floats};
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::*};

/// `FromForwardUpPos` call sites and their counts, and calls seen.
static CALLERS: Mutex<(Vec<(usize, u64)>, u64)> = Mutex::new((Vec::new(), 0));
/// The latest update's stored and written target yaw (degrees, as f32 bits), for `probe_frames`.
static LAST_YAWS: AtomicU64 = AtomicU64::new(0);

/// `FromForwardUpPos` on `camera` with its `forward` argument, called from `site`.
pub fn seen(camera: usize, forward: usize, site: usize) {
    if !options().look {
        return;
    }
    let Ok(mut callers) = CALLERS.lock() else { return };
    if let Some((_, count)) = callers.0.iter_mut().find(|(s, _)| *s == site) {
        *count += 1;
    } else if callers.0.len() < 32 {
        callers.0.push((site, 1));
    }
    callers.1 += 1;
    let n = callers.1;
    if n % 120 != 1 || n > 600 {
        return;
    }
    drop(callers);
    let Some(f) = mem::read::<[f32; 3]>(forward) else { return };
    let pitch = f[1].clamp(-1.0, 1.0).asin();
    log!(
        "look probe {n}: call from {}, camera {camera:#x} {}, forward {f:?} (pitch {:.2} deg either sign)",
        Module::describe(site),
        class_name(camera).unwrap_or_default(),
        pitch.to_degrees()
    );
    if pitch.abs() < 0.1 {
        log!("look probe: pitch too small to match fields; look well up or down with the mouse");
        return;
    }
    // Objects the camera points to, then the player objects those point to (in DL2 the look
    // targets sat one level further, in the character), and the fields in each that hold the pitch.
    let mut seen = vec![camera];
    let report = |label: String, object: usize, class: &str| {
        let hits = find_floats(object, 0x3000, angle_matcher(pitch));
        let shown: Vec<String> = hits.iter().take(24).map(|(o, v, l)| format!("+{o:#x}={v:.3}({l})")).collect();
        log!("  {label} -> {object:#x} {class}: {}", if shown.is_empty() { "no pitch fields".into() } else { shown.join(" ") });
    };
    for slot in (0..0x100).step_by(8) {
        let Some(object) = mem::read::<usize>(camera + slot) else { continue };
        if seen.contains(&object) || !module::plausible_object(object, 2) {
            continue;
        }
        let Some(class) = class_name(object) else { continue };
        seen.push(object);
        report(format!("camera+{slot:#x}"), object, &class);
        // The camera may point into the middle of the object: scan from its start.
        let whole = complete_object(object).unwrap_or(object);
        if whole != object {
            log!("    (a subobject at +{:#x} of the complete object {whole:#x})", object - whole);
        }
        for inner in (0..0x3000).step_by(8) {
            let Some(target) = mem::read::<usize>(whole + inner) else { continue };
            if seen.contains(&target) || !module::plausible_object(target, 2) {
                continue;
            }
            let Some(inner_class) = class_name(target).filter(|c| c.contains("Player")) else { continue };
            seen.push(target);
            report(format!("  (camera+{slot:#x})'s object+{inner:#x}"), target, &inner_class);
        }
    }
}

/// One steering update of head aim (angles in degrees; yaws positive left, the targets as the
/// character keeps them).
pub struct Steer {
    pub frame: u64,
    pub camera_yaw: f32,
    pub facing: f32,
    pub aim_yaw: f32,
    pub aim_pitch: f32,
    /// The character's target yaw and pitch as the game left them, and as head aim wrote them.
    pub yaw: f32,
    pub target: f32,
    pub pitch: f32,
    pub target_pitch: f32,
    pub character: usize,
}

/// Head aim steered the character.
pub fn steered(s: &Steer) {
    LAST_YAWS.store(((s.yaw.to_bits() as u64) << 32) | s.target.to_bits() as u64, Relaxed);
    if options().look && (s.frame <= 6 || s.frame.is_multiple_of(120)) {
        log!(
            "head aim {}: camera yaw {:.2} (positive left), facing {:.2}, aim yaw {:.2} pitch {:.2}; target yaw {:.2} -> {:.2}, pitch {:.2} -> {:.2}; current pitch {:.2}",
            s.frame,
            s.camera_yaw,
            s.facing,
            s.aim_yaw,
            s.aim_pitch,
            s.yaw,
            s.target,
            s.pitch,
            s.target_pitch,
            mem::read::<f32>(s.character + crate::engine::CURRENT_PITCH).unwrap_or(f32::NAN)
        );
    }
}

/// The latest update's target yaw as the game had left it and as head aim wrote it.
pub fn last_yaws() -> (f32, f32) {
    let bits = LAST_YAWS.load(Relaxed);
    (f32::from_bits((bits >> 32) as u32), f32::from_bits(bits as u32))
}

/// `probe_cutscene`: one look update (degrees; `facing` none before head aim knows it).
#[allow(clippy::too_many_arguments)]
pub fn update(frame: u64, yaw: f32, pitch: f32, camera_yaw: f32, facing: Option<f32>, head_yaw: f32, movie: bool) {
    if options().cutscene && super::view::cutscene_line() {
        log!("cs look update {frame}: yaw read {yaw:.2} pitch read {pitch:.2} camera yaw {camera_yaw:.2} facing {facing:?} head {head_yaw:.2} movie {movie}");
    }
}

/// The call sites seen (end of a run).
pub fn report() {
    let Ok(callers) = CALLERS.lock() else { return };
    for (site, count) in &callers.0 {
        log!("FromForwardUpPos called from {} x{count}", Module::describe(*site));
    }
}
