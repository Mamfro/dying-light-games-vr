//! Provider: look fields (`eng_chr::providers::look_fields`), where The Beast keeps its look
//! targets and character (`engine.rs`), for head aim (`monaka_arms::look_targets`, its component
//! `crate::LOOK`). Head aim steers at the arms visual's camera-target callback
//! (`eng_chr::providers::rig`, hooked by the look: the callback has the arms visual, and so the
//! character; the camera setter only sees the renderer's camera, where the game's own camera is
//! noted before the eye camera replaces it); and the pitch lock's input (`monaka_arms::pitch_lock`,
//! its component `crate::PITCH_LOCK`).

use crate::Beast;
use crate::providers::engine;
use eng_chr::providers::look_fields::{Fields, HeadAim};
use monaka_arms::look_targets::{LooksByTargets, TargetAim};
use monaka_arms::pitch_lock::PitchInput;
use monaka_core::situation::Situation;
use monaka_producer::game::{Lack, Start};

/// Head aim on The Beast's player character.
pub static AIM: HeadAim = HeadAim::new(Fields {
    target_yaw: engine::TARGET_YAW,
    target_pitch: engine::TARGET_PITCH,
    player_vtable: engine::PLAYER_VTABLE,
    vis_vtable: engine::FPP_VIS_VTABLE,
    target_character: engine::TARGET_CHARACTER,
});

/// Head aim on the character (`monaka_arms::look_targets`), steered at the arms visual's
/// camera-target callback (the Chrome look provider's install hooks it).
impl LooksByTargets for Beast {
    const REPORTS: &'static [Situation] = eng_chr::feature::look_reports::<Beast>();

    fn aim() -> &'static TargetAim {
        AIM.target()
    }

    fn install(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
        eng_chr::feature::install_look(start)
    }

    fn release() {
        eng_chr::feature::release_look::<Beast>();
    }
}

/// The pitch lock (`monaka_arms::pitch_lock`) at the game's input-action converter, which head aim
/// hooks too (it is hooked here when head aim is off), on the game camera's own pitch (noted at the
/// player camera update, `crate::providers::cameras`), while the eyes are drawn.
impl PitchInput for Beast {
    fn install(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
        if start.is_on(&crate::LOOK) { Ok(()) } else { eng_chr::feature::hook_input_action(start) }
    }

    fn pitch() -> Option<f32> {
        AIM.game_pitch()
    }

    fn holds() -> bool {
        crate::providers::eye_site::running()
    }
}
