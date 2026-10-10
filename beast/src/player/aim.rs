//! Where the character looks while VR runs: head aim ([`eng_chr::headaim`]) on The Beast's
//! offsets (`engine.rs`), steered at the arms visual's camera-target callback ([`crate::player::hands`]:
//! the callback has the arms visual, and so the character; the camera setter only sees the
//! renderer's camera, where the game's own camera is noted before the eye camera replaces it).

use crate::engine::{self, InputActionFn};
use eng_chr::headaim::{Fields, HeadAim, IdLoad, is_pitch_action};
use monaka_arms::aim::AimSettings;
use monaka_core::protocol::HeadPose;
use monaka_hook::module::Module;
use monaka_hook::{InFlight, Original};

pub static INPUT_ACTION: Original<InputActionFn> = Original::new();

/// Head aim on The Beast's player character.
pub static AIM: HeadAim = HeadAim::new(Fields {
    target_yaw: engine::TARGET_YAW,
    target_pitch: engine::TARGET_PITCH,
    player_vtable: engine::PLAYER_VTABLE,
    vis_vtable: engine::FPP_VIS_VTABLE,
    target_character: engine::TARGET_CHARACTER,
});

pub fn configure(settings: AimSettings, gamedll: usize) {
    AIM.configure(settings, gamedll);
}

/// The player camera update in the renderer's camera setter (`renderer_camera`), before the eye
/// camera replaces the game's: notes the game camera. Returns the head yaw (radians) head aim baked
/// into it (from the previous update's targets: the share written then), while head aim steers.
pub fn note_camera(renderer_camera: usize, back: [f32; 3], up: [f32; 3], position: [f32; 3]) -> Option<f32> {
    AIM.note_player_camera(renderer_camera, back, up, position);
    AIM.current_share()
}

/// Steers the character behind the arms visual's camera target `target` (called right after the
/// camera was set) for the next update: the head's direction, or the aiming controller's. With no
/// head (VR stopping), the head's share goes back.
pub fn steer(target: usize, head: Option<HeadPose>) {
    if let Some(character) = AIM.character_of(target) {
        eng_chr::grabs::hold(character, &crate::engine::GRABS);
        AIM.steer(character, head, None);
    }
}

/// Hands the head's share back at the next update (waits up to `timeout_ms` for it).
pub fn release(timeout_ms: u64) {
    AIM.release(timeout_ms);
}

/// Checks the vertical look actions against the engine's action table (name and the id loaded
/// beside it).
pub fn pitch_actions_match(engine_module: &Module) -> Result<(), String> {
    eng_chr::headaim::pitch_actions_match(engine_module, &engine::PITCH_ACTIONS, IdLoad::EdxImm32)
}

/// The game's input-action converter: while head aim steers the character, the head alone pitches
/// it, so vertical look input (mouse and stick) gets its neutral value.
pub unsafe extern "system" fn input_action(binding: usize, receivers: usize, value: f32, source: bool, repeat: bool) {
    let _flight = InFlight::enter();
    let value = AIM.neutral_pitch_if_live(binding, value, |action| is_pitch_action(&engine::PITCH_ACTIONS, action));
    // SAFETY: forwards the game's own call.
    unsafe { INPUT_ACTION.get()(binding, receivers, value, source, repeat) }
}
