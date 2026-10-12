//! Where the character looks while VR runs: head aim ([`eng_chr::headaim`], the method's component
//! `eng_chr::feature::look`) on The Beast's offsets (`engine.rs`), steered at the arms visual's
//! camera-target callback ([`crate::arms::hands`]: the callback has the arms visual, and so the
//! character; the camera setter only sees the renderer's camera, where the game's own camera is
//! noted before the eye camera replaces it).

use crate::{Beast, arms::hands, engine, stereo};
use eng_chr::feature::gamedll;
use eng_chr::headaim::{Fields, HeadAim};
use monaka_core::protocol::HeadPose;
use monaka_producer::game::{Component, Lack, Missing, Start};

/// Head aim on the character (`eng_chr::feature::look`), steered at the arms visual's camera-target
/// callback, which has the character ([`hands`]).
pub const LOOK: Component<Beast> = Component { needs: &[&stereo::EYES], install: install_look, release: release_look, ..eng_chr::feature::look() };

fn install_look(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    eng_chr::feature::install_look(start)?;
    // SAFETY: the game DLL's hash was checked.
    unsafe { hands::hook(start.hooks, gamedll(start)?) }.missing()
}

fn release_look() {
    hands::report();
    monaka_arms::controllers::close();
}

/// The view stays level while head aim steers (`eng_chr::view_bob`).
pub const VIEW_BOB: Component<Beast> = Component { needs: &[&LOOK], ..eng_chr::feature::view_bob() };

/// Head aim on The Beast's player character.
pub static AIM: HeadAim = HeadAim::new(Fields {
    target_yaw: engine::TARGET_YAW,
    target_pitch: engine::TARGET_PITCH,
    player_vtable: engine::PLAYER_VTABLE,
    vis_vtable: engine::FPP_VIS_VTABLE,
    target_character: engine::TARGET_CHARACTER,
});

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
        AIM.steer(character, head, None);
    }
}
