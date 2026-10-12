//! Where the character looks while VR runs: head aim ([`eng_chr::headaim`]) on DL2's offsets,
//! steered at the player camera update, plus what only DL2 has: the pitch lock.
//!
//! The game sets its player camera once per update through `FromForwardUpPos`, called from
//! `gamedll+PLAYER_CAMERA_UPDATE`, built from the camera's target (camera+0x40, the arms visual's
//! `ICameraTarget` base, PlayerFppVis_PH) whose look comes from the player character
//! ([target-0x570], PlayerDI_PH) as degrees at +0xb90 (yaw) and +0xb94 (pitch, negative down).
//! The look update steers those toward targets at +0xb98/+0xb9c, which mouse and stick input
//! accumulate into; writing the look pitch alone is overwritten the same frame.
//!
//! The pitch lock (`lock_pitch`, without head aim) blocks vertical look instead, letting through
//! only input that brings the game camera back toward level. The game converts each input action
//! at `gamedll+INPUT_ACTION` (binding: action id at +0, flags at +0x10); a neutral value is 0, or
//! 1 with flag 0x10. From farmerarmor/DyingLight2VR VerticalLook.h (MIT).
//!
//! Its probe (`probe_cameras`) is in `research::cameras`, its trace (`AIM_LOG`) in `research::aim`.

use crate::engine::{self, FromForwardFn};
use crate::stereo::{self, head, scene};
use crate::{Dl2, game};
use eng_chr::headaim::{Fields, HeadAim, is_pitch_action};
use monaka_arms::aim::{TOOL, neutral_pitch};
use monaka_core::aim::Levelling;
use monaka_core::manifest::{Kind, Opt, line};
use monaka_hook::AtomicF32;
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::game::{Component, Lack, Start, Switch};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Head aim on the character (`eng_chr::feature::look`), steered at the player camera update
/// (`FromForwardUpPos`, [`from_forward`]).
pub const LOOK: Component<Dl2> = Component { needs: &[&stereo::EYES], install: install_look, ..eng_chr::feature::look() };

fn install_look(start: &mut Start<'_, Dl2>) -> Result<(), Lack> {
    eng_chr::feature::install_look(start)?;
    start.hooks.site(&FROM_FORWARD, &start.build.engine, &engine::FROM_FORWARD_UP_POS, from_forward)?;
    Ok(())
}

const LOCK_PITCH: Opt = line("lock_pitch", "Block stick pitch", "With mouse / stick aim: no looking up and down with the stick, except toward level.", Kind::Switch { default: false });

/// While head aim does not steer, vertical look passes only toward level ([`lock_pitch`], at the
/// input-action converter head aim hooks, or this component without it).
pub const PITCH: Component<Dl2> = Component {
    name: "pitch",
    options: &[LOCK_PITCH],
    needs: &[&stereo::EYES],
    switch: Some(Switch::new("lock_pitch", false, "vertical look as the game has it")),
    install: install_pitch,
    ..Component::NONE
};

fn install_pitch(start: &mut Start<'_, Dl2>) -> Result<(), Lack> {
    eng_chr::headaim::pitch_rule(lock_pitch);
    if start.is_on(&LOOK) { Ok(()) } else { eng_chr::feature::hook_input_action(start) }
}

pub const VIEW_BOB: Component<Dl2> = Component { needs: &[&LOOK], ..eng_chr::feature::view_bob() };

pub static FROM_FORWARD: Original<FromForwardFn> = Original::new();

/// Head aim on DL2's player character.
pub static AIM: HeadAim = HeadAim::new(Fields {
    target_yaw: engine::TARGET_YAW,
    target_pitch: engine::TARGET_PITCH,
    player_vtable: engine::PLAYER_VTABLE,
    vis_vtable: engine::FPP_VIS_VTABLE,
    target_character: engine::TARGET_CHARACTER,
});

/// The game camera's own pitch (radians, positive up), for the pitch lock. With head aim the render
/// camera carries the head's pitch too, so the player camera update supplies the mouse's own pitch
/// instead (`MOUSE_PITCH_KNOWN`).
pub static GAME_PITCH: AtomicF32 = AtomicF32::zero();
pub static GAME_PITCH_KNOWN: AtomicBool = AtomicBool::new(false);
pub static MOUSE_PITCH_KNOWN: AtomicBool = AtomicBool::new(false);
pub static LEVELLING: AtomicU64 = AtomicU64::new(0);
static PITCH_LOCKED: AtomicU64 = AtomicU64::new(0);
/// The pitch lock's rule (radians): vertical look passes only back toward level, from beyond 1.5
/// degrees. The game writes its own pitch, so nothing is stepped.
const LEVEL: Levelling = Levelling { dead_band: 0.026, step: 0.0 };

/// Vertical look inputs neutralised (by head aim) or blocked (by the pitch lock).
pub fn pitch_blocked() -> u64 {
    AIM.pitch_blocked() + PITCH_LOCKED.load(Ordering::Relaxed)
}

/// Turns a levelled game camera back by the head yaw head aim baked into frame `counter`, so a head
/// turn is not counted twice.
pub fn turn_out_head_yaw(view: &mut [f32; 12], counter: u32) {
    crate::research::aim::view(view, counter);
    AIM.turn_out(view, Some(counter as u64));
}

/// `IBaseCamera::FromForwardUpPos`; its "forward" argument is the camera's backward axis. Four
/// arguments, so no shim: the stack walk finds the caller (a few calls per frame).
pub unsafe extern "system" fn from_forward(camera: usize, forward: *const f32, up: *const f32, position: *const f32) {
    let _guard = InFlight::enter();
    let game = game::get();
    if let Some(gamedll) = game.gamedll {
        let site = monaka_hook::probe::caller();
        if site == gamedll + engine::PLAYER_CAMERA_UPDATE
            && let Some(f) = mem::read::<[f32; 3]>(forward as usize)
        {
            player_camera_update(camera, f);
            if let (Some(u), Some(p)) = (mem::read::<[f32; 3]>(up as usize), mem::read::<[f32; 3]>(position as usize))
                && let Some(matrix) = AIM.note_player_camera(camera, f, u, p)
            {
                crate::research::cameras::player_camera(&matrix);
            }
        } else {
            crate::research::cameras::other_caller(camera, site);
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
    let head = scene::vr_active().then(|| head::HEAD.current()).flatten();
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
        Some(steered) => {
            GAME_PITCH.store(steered.own.pitch.to_radians());
            GAME_PITCH_KNOWN.store(true, Ordering::Release);
            MOUSE_PITCH_KNOWN.store(true, Ordering::Release);
            crate::research::aim::steered(&steered, counter, camera_pitch);
        }
        None if horizontal > 1e-4 => {
            // VR stopped (or no head): the game's own pitch.
            GAME_PITCH.store(camera_pitch);
            GAME_PITCH_KNOWN.store(true, Ordering::Release);
            MOUSE_PITCH_KNOWN.store(true, Ordering::Release);
        }
        None => {}
    }
}

/// Vertical look while head aim does not steer, with `lock_pitch`: the converter only runs on real
/// input, so it cannot steer by itself; input that turns the camera back toward level (beyond a 1.5
/// degree dead band) passes, the rest gets its neutral value.
pub fn lock_pitch(binding: usize, value: f32) -> f32 {
    let Some(neutral) = neutral_pitch(binding, |action| is_pitch_action(&engine::PITCH_ACTIONS, action)).filter(|_| scene::vr_active()) else { return value };
    let action = mem::read::<u32>(binding).unwrap_or(0);
    let toward = GAME_PITCH_KNOWN.load(Ordering::Acquire) && {
        let up = action == engine::PITCH_ACTIONS[0].2 || action == engine::PITCH_ACTIONS[2].2;
        LEVEL.toward_level(GAME_PITCH.load(), up)
    };
    if toward {
        if neutral != value {
            LEVELLING.fetch_add(1, Ordering::Relaxed);
        }
        return value;
    }
    if neutral != value {
        PITCH_LOCKED.fetch_add(1, Ordering::Relaxed);
    }
    neutral
}
