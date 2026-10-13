//! Provider: look fields, the look targets the look by a facing the mod owns (`monaka_body::look`)
//! reads and writes.
//!
//! Riptide's look provider: where the player's look targets live ([`FIELDS`], their offsets in
//! `engine.rs`: the character's wanted look), and the hook where the game builds the player camera
//! from them (`FromForwardUpPos`, [`from_forward`]). Each player camera update is handed to head
//! aim (`monaka_body::look::facing::steer`), which says what the targets become. Dying Light 1 also
//! holds the vertical look input at its input-action converter; Riptide's input runs on Dead
//! Island's own system, which has no such converter, so here the head's steering at each update is
//! all there is.

use crate::providers::engine::{self, FromForwardFn};
use crate::providers::eye_site;
use crate::{Facts, Riptide};
use eng_chr::providers::look_fields::Cutscene;
use monaka_body::look::facing::{self, Facing, PlayerUpdate, TargetUnits};
use monaka_body::look::{Delivery, Looks, StandDown};
use monaka_core::aim::Convention;
use monaka_core::situation::Situation;
use monaka_hook::module::Module;
use monaka_hook::probe::{caller, class_name, complete_object};
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::game::{Lack, Start};
use monaka_producer::{Rejection, log};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};

/// Where Riptide keeps the player's look targets.
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

/// Riptide's mapping (see `engine.rs`): the yaw turns right as it grows, as in Dying Light 1.
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
/// camera follows the targets: Dying Light 1's, whose `PlayerDI` Riptide's shares.
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

/// Riptide's look by a facing the mod owns: the character turned by the head at
/// `FromForwardUpPos` ([`from_forward`]).
impl Looks for Riptide {
    const DELIVERY: Delivery<Self> = Delivery::Facing(Facing { units: UNITS, read: target_yaw, write: set_targets, command: no_command });
    const REPORTS: &'static [Situation] = &[Situation::CameraTaken];

    fn install(start: &mut Start<'_, Riptide>) -> Result<(), Lack> {
        let Facts { engine: engine_module, .. } = start.build;
        hook_from_forward(start.hooks, engine_module)?;
        CUTSCENE.resolve(engine_module).map_err(Rejection::revision)?;
        LOOK_STEERS.store(true, Release);
        Ok(())
    }

    fn stand_down(character: usize) -> StandDown {
        let asked = CUTSCENE.ask(character);
        StandDown { now: asked.cutscene, changed: asked.changed }
    }
}

/// Head aim asks the hand rig what the hands hold (`facing::steer`); this game has no hand rig yet,
/// so the hands hold nothing it knows of (the trait's defaults). The rig's description is Dying
/// Light 1's and is never installed: no component here poses the arms.
impl monaka_body::hand_rig::HandRig for Riptide {
    const RIG: monaka_body::hand_rig::Rig =
        eng_chr::providers::fpp::chrome_rig(monaka_body::BoneAxes::DYING_LIGHT_1, None, monaka_body::hand_rig::Climbing::None);
}

/// The character's target yaw now, if readable.
fn target_yaw(character: usize) -> Option<f32> {
    targets(character).0
}

/// No spots to save or come back to in this game.
fn no_command(_: u64, _: usize, _: f32) -> Option<f32> {
    None
}

/// Logs where the aim came from (end of a run).
pub fn report() {
    log!("player camera updates steered: {}", STEERED.load(Relaxed));
    facing::report();
}

/// The player camera update (head aim then knows the player camera), the look the camera was
/// built from, and the head pose the eyes of the frame will be built with.
fn player_update(camera: usize, forward: usize) -> Option<PlayerUpdate> {
    let character = (FIELDS.character)(camera)?;
    eye_site::set_player_camera(camera);
    let (Some(yaw), Some(pitch)) = targets(character) else { return None };
    // The camera's argument is its backward axis (as in Dying Light 1 and 2): look = -argument; its
    // yaw is the levelled camera's.
    let camera_yaw = mem::read::<[f32; 3]>(forward).map(|f| f[0].atan2(f[2])).filter(|y| y.is_finite())?;
    Some(PlayerUpdate { character, yaw, pitch, camera_yaw, head: eye_site::pending_pose() })
}

/// Whether `site` is the call that sets the player camera.
fn is_player_call(site: usize) -> bool {
    Module::find(FIELDS.player_call.0).is_some_and(|m| site == m.at(FIELDS.player_call.1))
}

pub static FROM_FORWARD_ORIGINAL: Original<FromForwardFn> = Original::new();
/// Head aim steers at the player camera updates: its look hooked them.
static LOOK_STEERS: AtomicBool = AtomicBool::new(false);
/// Player camera updates handed to head aim.
static STEERED: AtomicU64 = AtomicU64::new(0);

/// The player camera's setter, where head aim steers.
pub(crate) fn hook_from_forward(hooks: &mut Hooks, engine_module: &Module) -> Result<(), Lack> {
    hooks.site(&FROM_FORWARD_ORIGINAL, engine_module, &engine::FROM_FORWARD, from_forward)?;
    Ok(())
}

/// `IBaseCamera::FromForwardUpPos(forward, up, position)`.
pub unsafe extern "system" fn from_forward(camera: *mut core::ffi::c_void, forward: *const f32, up: *const f32, position: *const f32) {
    let _flight = InFlight::enter();
    let mut player = false;
    if eye_site::capturing() && eye_site::config().aim.head && LOOK_STEERS.load(Acquire) {
        player = is_player_call(caller());
        if player && let Some(update) = player_update(camera as usize, forward as usize) {
            STEERED.fetch_add(1, Relaxed);
            facing::steer::<Riptide>(update);
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { FROM_FORWARD_ORIGINAL.get()(camera, forward, up, position) };
    if player {
        // The player camera is built: turned to the view before the game lays out its HUD.
        eye_site::turn_after_game_update();
    }
}
