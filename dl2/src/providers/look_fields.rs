//! Provider: look fields (`eng_chr::providers::look_fields`), where DL2 keeps its look targets and
//! character, for head aim (`monaka_body::look`, its component `crate::LOOK`), steered at
//! the player camera update ([`from_forward`]); and the pitch lock's input (`monaka_body::pitch_lock`,
//! its component `crate::PITCH_LOCK`) on the game camera's own pitch, noted at that update.
//!
//! The game sets its player camera once per update through `FromForwardUpPos`, called from
//! `gamedll+PLAYER_CAMERA_UPDATE`, built from the camera's target (camera+0x40, the arms visual's
//! `ICameraTarget` base, PlayerFppVis_PH) whose look comes from the player character
//! ([target-0x570], PlayerDI_PH) as degrees at +0xb90 (yaw) and +0xb94 (pitch, negative down).
//! The look update steers those toward targets at +0xb98/+0xb9c, which mouse and stick input
//! accumulate into; writing the look pitch alone is overwritten the same frame.
//!
//! Its probe (`probe_cameras`) is in `research::cameras`, its trace (`AIM_LOG`) in `research::aim`.

use crate::Dl2;
use crate::providers::engine::{self, FromForwardFn};
use crate::providers::game;
use monaka_stereo::same_frame::PAIRS;
use eng_chr::providers::look_fields::HeadAim;
use eng_chr::providers::look_fields::Fields;
use monaka_body::look::aim::TOOL;
use monaka_hook::{InFlight, Original, mem};
use monaka_body::look::{Carried, Delivery, Looks};
use monaka_body::pitch_lock::PitchInput;
use monaka_core::situation::Situation;
use monaka_producer::game::{Lack, Start};

/// Head aim on the character (`monaka_body::look`, into its look targets), steered at the player
/// camera update (`FromForwardUpPos`, [`from_forward`]): the camera of a game frame is built from
/// the targets as that frame's update began.
impl Looks for Dl2 {
    const DELIVERY: Delivery<Self> = Delivery::Targets { carried: Carried::AtUpdate };
    const REPORTS: &'static [Situation] = eng_chr::feature::look_reports::<Dl2>();

    fn install(start: &mut Start<'_, Dl2>) -> Result<(), Lack> {
        eng_chr::feature::install_look(start)?;
        start.hooks.site(&FROM_FORWARD, &start.build.engine, &engine::FROM_FORWARD_UP_POS, from_forward)?;
        Ok(())
    }

    fn release() {
        eng_chr::feature::release_look::<Dl2>();
    }
}

/// The pitch lock (`monaka_body::pitch_lock`) at the game's input-action converter, which head aim
/// hooks too (it is hooked here when head aim is off), on the game camera's own pitch
/// ([`HeadAim::game_pitch`](eng_chr::providers::look_fields::HeadAim::game_pitch)), while the eyes
/// are drawn.
impl PitchInput for Dl2 {
    fn install(start: &mut Start<'_, Dl2>) -> Result<(), Lack> {
        if start.is_on(&crate::LOOK) { Ok(()) } else { eng_chr::feature::hook_input_action(start) }
    }

    fn pitch() -> Option<f32> {
        AIM.game_pitch()
    }

    fn holds() -> bool {
        PAIRS.running()
    }
}

pub static FROM_FORWARD: Original<FromForwardFn> = Original::new();

/// Head aim on DL2's player character.
pub static AIM: HeadAim = HeadAim::new(Fields {
    target_yaw: engine::TARGET_YAW,
    target_pitch: engine::TARGET_PITCH,
    player_vtable: engine::PLAYER_VTABLE,
    vis_vtable: engine::FPP_VIS_VTABLE,
    target_character: engine::TARGET_CHARACTER,
    camera_sources: None,
});

/// `IBaseCamera::FromForwardUpPos`; its "forward" argument is the camera's backward axis. Four
/// arguments, so no shim: the stack walk finds the caller (a few calls per frame).
pub unsafe extern "system" fn from_forward(camera: usize, forward: *const f32, up: *const f32, position: *const f32) {
    let _guard = InFlight::enter();
    let game = game::get();
    if let Some(gamedll) = game.gamedll {
        let site = monaka_hook::probe::caller();
        match AIM.player_update(gamedll + engine::PLAYER_CAMERA_UPDATE, site, camera, forward as usize, up as usize, position as usize) {
            Some(update) => {
                player_camera_update(camera, update.back);
                if let Some(matrix) = update.matrix {
                    crate::research::cameras::player_camera(&matrix);
                }
            }
            None => crate::research::cameras::other_caller(camera, site),
        }
    }
    // SAFETY: the original, with the game's own arguments.
    unsafe { FROM_FORWARD.get()(camera, forward, up, position) }
}

fn player_camera_update(camera: usize, forward: [f32; 3]) {
    let horizontal = (forward[0] * forward[0] + forward[2] * forward[2]).sqrt();
    // The argument is the backward axis.
    let camera_pitch = if horizontal > 1e-4 { -forward[1].atan2(horizontal) } else { 0.0 };
    let counter = game::get().counter().map(|(_, c)| c as u64);
    let character = mem::read::<usize>(camera + engine::CAMERA_TARGET).filter(|&t| t != 0).and_then(|target| AIM.character_of(target));
    let head = PAIRS.running().then(|| PAIRS.head().current()).flatten();
    let steered = match character {
        Some(character) => {
            AIM.steer(character, head, counter)
        }
        None => {
            TOOL.release();
            None
        }
    };
    match steered {
        // Head aim noted the character's own pitch as it steered.
        Some(steered) => crate::research::aim::steered(&steered, counter, camera_pitch),
        // VR stopped (or no head): the game's own pitch.
        None if horizontal > 1e-4 => AIM.note_camera_pitch(camera_pitch),
        None => {}
    }
}
