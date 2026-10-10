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

use crate::config;
use crate::engine::{self, FromForwardFn, InputActionFn};
use crate::game;
use crate::{view::head, view::scene};
use eng_chr::headaim::{Fields, HeadAim, IdLoad, is_pitch_action};
use monaka_arms::aim::{TOOL, neutral_pitch};
use monaka_hook::AtomicF32;
use monaka_hook::module::Module;
use monaka_hook::{InFlight, Original, mem};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub static FROM_FORWARD: Original<FromForwardFn> = Original::new();
pub static INPUT_ACTION: Original<InputActionFn> = Original::new();

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
            eng_chr::grabs::hold(character, &engine::GRABS);
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

/// Waits briefly for the next player camera update to hand the head's share back; reports what is
/// left (the game paused: it stays).
pub fn release(timeout_ms: u64) {
    AIM.release(timeout_ms);
}

pub unsafe extern "system" fn input_action(binding: usize, receivers: usize, mut value: f32, source: bool, repeat: bool) {
    let _guard = InFlight::enter();
    let is_pitch = |action: u32| is_pitch_action(&engine::PITCH_ACTIONS, action);
    if scene::vr_active() {
        if AIM.live() {
            // While head aim steers the character, the head alone pitches it.
            value = AIM.neutral_pitch_if_live(binding, value, is_pitch);
        } else if config::get().lock_pitch
            && let Some(neutral) = neutral_pitch(binding, is_pitch)
        {
            // The converter only runs on real input, so it cannot steer by itself: input that
            // turns the camera back toward level (beyond a 1.5 degree dead band) passes.
            let action = mem::read::<u32>(binding).unwrap_or(0);
            let toward = GAME_PITCH_KNOWN.load(Ordering::Acquire) && {
                let pitch = GAME_PITCH.load();
                let up = action == engine::PITCH_ACTIONS[0].2 || action == engine::PITCH_ACTIONS[2].2;
                (up && pitch < -0.026) || (!up && pitch > 0.026)
            };
            if toward {
                if neutral != value {
                    LEVELLING.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                if neutral != value {
                    PITCH_LOCKED.fetch_add(1, Ordering::Relaxed);
                }
                value = neutral;
            }
        }
    }
    // SAFETY: the original, with the game's arguments (the value possibly neutralised).
    unsafe { INPUT_ACTION.get()(binding, receivers, value, source, repeat) }
}

/// Checks the vertical look action ids against the engine's own action names.
pub fn pitch_actions_match(engine_module: &Module) -> Result<(), String> {
    eng_chr::headaim::pitch_actions_match(engine_module, &engine::PITCH_ACTIONS, IdLoad::MemImm32)
}
