//! Provider: look fields, the look targets the look by a facing the mod owns (`monaka_body::look`)
//! reads and writes.
//!
//! DL1's look provider: where the player's look targets live ([`FIELDS`], their offsets in
//! `engine.rs`: the character's wanted look), the hook where the game builds the player camera from
//! them (`FromForwardUpPos`, [`from_forward`]), and the input actions that pitch the look
//! ([`input_action`]). Each player camera update is handed to head aim
//! (`monaka_body::look::facing::steer`), which says what the targets become; without head aim the
//! pitch lock (`monaka_body::pitch_lock`) reads the camera's pitch there and holds the look input.

use crate::providers::engine::{self, FromForwardFn};
use crate::extras::spots;
use crate::{Dl1, Facts};
use eng_chr::providers::look_fields::Cutscene;
use monaka_body::look::facing::{self, Facing, PlayerUpdate, TargetUnits};
use monaka_body::look::{Delivery, Looks, StandDown};
use monaka_core::situation::Situation;
use monaka_body::pitch_lock::PitchInput;
use monaka_producer::game::{Lack, Start};
use monaka_producer::{Rejection, log};
use crate::providers::eye_site;
use monaka_core::aim::Convention;
use monaka_hook::module::Module;
use monaka_hook::probe::{caller, class_name, complete_object};
use monaka_hook::{Hooks, InFlight, Original, mem};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::*};

/// Where DL1 keeps the player's look targets.
pub struct LookFields {
    /// The module and offset of the call that sets the player camera (`CameraFPPDI::Update`'s
    /// `FromForwardUpPos` call, by its return address).
    pub player_call: (&'static str, usize),
    /// From that camera to the character holding the look targets.
    pub character: fn(camera: usize) -> Option<usize>,
    pub yaw: usize,
    pub pitch: usize,
    pub convention: Convention,
}

/// DL1's mapping (see `engine.rs`): the yaw turns right as it grows, as in DL2.
pub const FIELDS: LookFields = LookFields {
    player_call: (engine::GAMEDLL, engine::PLAYER_CAMERA_CALL),
    character,
    yaw: engine::TARGET_YAW,
    pitch: engine::TARGET_PITCH,
    convention: Convention::DEGREES_YAW_RIGHT,
};

/// The character's vtable, once its class name has been checked.
static CHARACTER_VTABLE: AtomicU64 = AtomicU64::new(0);

/// From the player camera to its character, checked to be a `PlayerDI`.
fn character(camera: usize) -> Option<usize> {
    let vis = mem::read::<usize>(camera + engine::CAMERA_FPP_VIS)?;
    let whole = complete_object(vis)?;
    let character = mem::read::<usize>(whole + engine::FPP_VIS_CHARACTER)?;
    let vtable = mem::read::<usize>(character)? as u64;
    let known = CHARACTER_VTABLE.load(Relaxed);
    if known == 0 && class_name(character).as_deref() == Some(engine::CHARACTER_CLASS) {
        CHARACTER_VTABLE.store(vtable, Relaxed);
        return Some(character);
    }
    (known != 0 && known == vtable).then_some(character)
}

/// The camera's yaw (degrees, positive left) for a target yaw `t` is this minus `t` whenever the
/// camera follows the targets (in play and on ledges alike).
const CAMERA_YAW_OF_TARGET: f32 = -90.0;

/// The character's look targets now: (yaw, pitch), each if readable.
pub fn targets(character: usize) -> (Option<f32>, Option<f32>) {
    (mem::read::<f32>(character + FIELDS.yaw), mem::read::<f32>(character + FIELDS.pitch))
}

/// Sets the character's look targets (yaw, then pitch).
pub fn set_targets(character: usize, yaw: f32, pitch: f32) {
    mem::write(character + FIELDS.yaw, yaw);
    mem::write(character + FIELDS.pitch, pitch);
}

/// Whether a cutscene has the player (`eng_chr::providers::look_fields::Cutscene`: the engine's own
/// `IsObjectOnSomeMovie` on the character, a `PlayerDI`, asked at every update); head aim needs it.
static CUTSCENE: Cutscene = Cutscene::new();

/// The look targets' units: degrees, the yaw turning right as it grows; a target yaw of `t` looks
/// [`CAMERA_YAW_OF_TARGET`] minus `t` in the world.
const UNITS: TargetUnits = TargetUnits { convention: FIELDS.convention, yaw_zero: CAMERA_YAW_OF_TARGET, turn: 360.0 };

/// DL1's look by a facing the mod owns: the character turned by the head at `FromForwardUpPos`
/// ([`from_forward`]), the look input actions held ([`input_action`]); spots to come back to are
/// taken at its updates ([`spots`]).
impl Looks for Dl1 {
    const DELIVERY: Delivery<Self> = Delivery::Facing(Facing { units: UNITS, read: target_yaw, write: set_targets, command: facing_command });
    const REPORTS: &'static [Situation] = &[Situation::CameraTaken, Situation::Traversal];

    fn install(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
        let Facts { engine: engine_module, gamedll, .. } = start.build;
        spots::install(engine_module, start.session.dir())?;
        hook_from_forward(start.hooks, engine_module)?;
        look_actions_match(gamedll).map_err(Rejection::revision)?;
        CUTSCENE.resolve(engine_module).map_err(Rejection::revision)?;
        hook_input_action(start.hooks, gamedll)?;
        LOOK_STEERS.store(true, Release);
        Ok(())
    }

    fn stand_down(character: usize) -> StandDown {
        let asked = CUTSCENE.ask(character);
        StandDown { now: asked.cutscene, changed: asked.changed }
    }
}

/// The character's target yaw now, if readable.
fn target_yaw(character: usize) -> Option<f32> {
    targets(character).0
}

/// A spot the player saved or asked to come back to at this update, given the facing now.
fn facing_command(now: u64, character: usize, facing: f32) -> Option<f32> {
    match spots::take_command(now)? {
        spots::Command::Save(name) => {
            spots::save(&name, character, facing);
            None
        }
        spots::Command::Go(name) => spots::go(&name, character),
    }
}

/// Logs the vertical look inputs neutralised and where the aim came from (end of a run).
pub fn report() {
    log!("vertical look inputs neutralised: {}", pitch_inputs_blocked());
    facing::report();
}

/// (head aim then knows the player camera), the look the camera was built from, and the head pose
/// the eyes of the frame will be built with.
fn player_update(camera: usize, forward: usize) -> Option<PlayerUpdate> {
    let character = (FIELDS.character)(camera)?;
    eye_site::set_player_camera(camera);
    let (Some(yaw), Some(pitch)) = targets(character) else { return None };
    // The camera's argument is its backward axis (DL2 and DL1): look = -argument; its yaw is the
    // levelled camera's.
    let camera_yaw = mem::read::<[f32; 3]>(forward).map(|f| f[0].atan2(f[2])).filter(|y| y.is_finite())?;
    Some(PlayerUpdate { character, yaw, pitch, camera_yaw, head: eye_site::pending_pose() })
}

/// Whether `site` is the call that sets the player camera.
fn is_player_call(site: usize) -> bool {
    Module::find(FIELDS.player_call.0).is_some_and(|m| site == m.at(FIELDS.player_call.1))
}

pub static FROM_FORWARD_ORIGINAL: Original<FromForwardFn> = Original::new();
pub static INPUT_ACTION_ORIGINAL: Original<engine::InputActionFn> = Original::new();
static PITCH_INPUTS_BLOCKED: AtomicU64 = AtomicU64::new(0);
/// Head aim steers at the player camera updates: its look hooked them (the pitch lock hooks them
/// without it).
static LOOK_STEERS: AtomicBool = AtomicBool::new(false);
/// The game camera's pitch as the latest player camera update set it (radians, positive up, f32
/// bits; NaN: not known).
static CAMERA_PITCH: AtomicU32 = AtomicU32::new(u32::MAX);

/// The pitch lock (`monaka_body::pitch_lock`) at the game's input-action converter, on the game
/// camera's pitch as each player camera update sets it, while the eyes are drawn. Head aim hooks
/// both ([`Looks::install`]); without it they are hooked here.
impl PitchInput for Dl1 {
    fn install(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
        if start.is_on(&crate::LOOK) {
            return Ok(());
        }
        LOOK_STEERS.store(false, Release);
        let Facts { engine: engine_module, gamedll, .. } = start.build;
        look_actions_match(gamedll).map_err(Rejection::revision)?;
        hook_from_forward(start.hooks, engine_module)?;
        hook_input_action(start.hooks, gamedll)?;
        Ok(())
    }

    fn pitch() -> Option<f32> {
        Some(f32::from_bits(CAMERA_PITCH.load(Relaxed))).filter(|pitch| !pitch.is_nan())
    }

    fn holds() -> bool {
        eye_site::capturing()
    }

    /// The input manager's pad last used: `-1` after the keyboard or mouse, so the mouse keeps its
    /// vertical look.
    fn stick_in_use() -> bool {
        let Some(gamedll) = Module::find(engine::GAMEDLL) else { return true };
        let manager = mem::read::<usize>(gamedll.at(engine::GAME))
            .filter(|&game| game != 0)
            .and_then(|game| mem::read::<usize>(game + engine::MANAGER_IN_GAME))
            .filter(|&manager| manager != 0);
        manager.and_then(|manager| mem::read::<i32>(manager + engine::MANAGER_LAST_PAD)).is_none_or(|pad| pad >= 0)
    }
}

/// The camera's pitch (radians, positive up) from its setter's "forward" argument (its backward
/// axis), none looking straight up or down.
fn camera_pitch(back: [f32; 3]) -> Option<f32> {
    let horizontal = (back[0] * back[0] + back[2] * back[2]).sqrt();
    (horizontal > 1e-4).then(|| -back[1].atan2(horizontal))
}

/// Whether `action` turns the camera up (look up, rotate up).
fn turns_up(action: u32) -> bool {
    [0, 2].into_iter().any(|i| engine::LOOK_ACTIONS[i].2 == action)
}

/// The player camera's setter, where head aim steers (and its probe watches).
pub(crate) fn hook_from_forward(hooks: &mut Hooks, engine_module: &Module) -> Result<(), Lack> {
    hooks.site(&FROM_FORWARD_ORIGINAL, engine_module, &engine::FROM_FORWARD, from_forward)?;
    Ok(())
}

/// The game's input-action converter, where the vertical look input is held neutral.
pub(crate) fn hook_input_action(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Lack> {
    hooks.site(&INPUT_ACTION_ORIGINAL, gamedll, &engine::INPUT_ACTION, input_action)?;
    Ok(())
}

/// Checks DL1's vertical look actions against the game's own action table (name and id).
pub fn look_actions_match(gamedll: &Module) -> Result<(), String> {
    engine::LOOK_ACTIONS.into_iter().try_for_each(|action| engine::action_matches(gamedll, action))
}

/// The vertical look inputs given their neutral value so far.
pub fn pitch_inputs_blocked() -> u64 {
    PITCH_INPUTS_BLOCKED.load(Relaxed)
}

/// The game's input-action converter (`InputAction::Execute`). While head aim steers the
/// character, the head alone pitches it: vertical look input (mouse and stick) gets its neutral
/// value; otherwise the pitch lock passes it only back toward level (`monaka_body::pitch_lock`,
/// when on).
pub unsafe extern "system" fn input_action(action: *mut core::ffi::c_void, targets: *mut core::ffi::c_void, value: f32, flag: u8) {
    let _flight = InFlight::enter();
    let mut value = value;
    if let Some(neutral) = monaka_body::look::aim::neutral_pitch(action as usize, |id| engine::LOOK_ACTIONS.iter().any(|&(_, _, look)| look == id)) {
        if eye_site::capturing() && facing::live() {
            if value != neutral {
                PITCH_INPUTS_BLOCKED.fetch_add(1, Relaxed);
            }
            value = neutral;
        } else {
            value = monaka_body::pitch_lock::pass(value, neutral, turns_up(mem::read::<u32>(action as usize).unwrap_or(0)));
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { INPUT_ACTION_ORIGINAL.get()(action, targets, value, flag) }
}

/// `IBaseCamera::FromForwardUpPos(forward, up, position)`.
pub unsafe extern "system" fn from_forward(camera: *mut core::ffi::c_void, forward: *const f32, up: *const f32, position: *const f32) {
    let _flight = InFlight::enter();
    let mut player = false;
    if eye_site::capturing() {
        let config = eye_site::config();
        let player_call = is_player_call(caller());
        if player_call && let Some(pitch) = mem::read::<[f32; 3]>(forward as usize).and_then(camera_pitch) {
            CAMERA_PITCH.store(pitch.to_bits(), Relaxed);
        }
        if config.aim.head && LOOK_STEERS.load(Acquire) {
            player = player_call;
            if player && let Some(update) = player_update(camera as usize, forward as usize) {
                facing::steer::<Dl1>(update);
            }
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { FROM_FORWARD_ORIGINAL.get()(camera, forward, up, position) };
    if player {
        // The player camera is built: turned to the view before the game lays out its HUD.
        eye_site::turn_after_game_update();
    }
}

#[cfg(test)]
mod lift_checks;
