//! Head aim for Dying Light 1. Head aim owns which way the view faces ([`State::facing`]): the
//! right stick turns it, the view is it plus the head, at the game camera's position, and the
//! character's look targets are steered to it plus the head (or the aiming hand). A cutscene is
//! the engine's own state ([`on_movie`]); then the view is its camera plus the head.
//!
//! DL1's look fields ([`LOOK`]) have their offsets in `engine.rs`; `probe_look`
//! (`research::look`) finds them, the way it does for DL2.

use crate::engine::{self, FromForwardFn};
use crate::view::stereo;
use monaka_arms::aim::TOOL;
use monaka_core::aim::{BakedYaw, Convention};
use monaka_core::camera::head_yaw_pitch;
use monaka_core::math::{wrap_degrees as wrapped_degrees, wrap_radians as wrapped};
use monaka_hook::module::Module;
use monaka_hook::probe::{caller, class_name, complete_object};
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};

/// Where DL1 keeps the player's look targets.
pub struct LookFields {
    /// The module and offset of the call that sets the player camera (the busiest
    /// `FromForwardUpPos` call site during play).
    pub player_call: (&'static str, usize),
    /// From that camera to the character holding the look targets.
    pub character: fn(camera: usize) -> Option<usize>,
    pub yaw: usize,
    pub pitch: usize,
    pub convention: Convention,
}

/// DL1's mapping (see `engine.rs`): the yaw turns right as it grows, as in DL2.
pub const LOOK: LookFields = LookFields {
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

pub static FROM_FORWARD_ORIGINAL: Original<FromForwardFn> = Original::new();
pub static INPUT_ACTION_ORIGINAL: Original<engine::InputActionFn> = Original::new();
static PITCH_INPUTS_BLOCKED: AtomicU64 = AtomicU64::new(0);

/// Checks DL1's vertical look actions against the game's own action table (name and id).
pub fn look_actions_match(gamedll: &Module) -> Result<(), String> {
    for (entry, name, id) in engine::LOOK_ACTIONS {
        let at = gamedll.at(entry);
        let found = mem::read::<usize>(at).and_then(|text| mem::read_c_string(text, 64));
        let found_id = mem::read::<u32>(at + 0x10);
        if found.as_deref() != Some(name) || found_id != Some(id) {
            return Err(format!("action table entry {entry:#x} is {found:?} id {found_id:?}, not {name} id {id}"));
        }
    }
    Ok(())
}

/// The game's input-action converter. While head aim steers the character, the head alone
/// pitches it: vertical look input (mouse and stick) gets its neutral value.
pub unsafe extern "system" fn input_action(binding: *mut core::ffi::c_void, receivers: *mut core::ffi::c_void, value: f32, source: u8, repeat: u8) {
    let _flight = InFlight::enter();
    let mut value = value;
    if stereo::capturing()
        && live()
        && let Some(neutral) = monaka_arms::aim::neutral_pitch(binding as usize, |action| engine::LOOK_ACTIONS.iter().any(|&(_, _, id)| id == action))
    {
        if value != neutral {
            PITCH_INPUTS_BLOCKED.fetch_add(1, Relaxed);
        }
        value = neutral;
    }
    // SAFETY: forwards the game's own call.
    unsafe { INPUT_ACTION_ORIGINAL.get()(binding, receivers, value, source, repeat) }
}

/// A climb under way ([`monaka_arms::traverse`]). Yaws in the world: radians, positive left.
#[derive(Clone, Copy)]
struct Climb {
    /// The game camera's yaw at the last update.
    camera: f32,
    /// The right stick head aim pushes (-1..=1, x right).
    push: f32,
    /// When the look was last traced.
    traced: u64,
}

/// Head aim's stick turns the game camera toward where the head looks once they are this far
/// apart (radians, 3 degrees)...
const PUSH_FROM: f32 = 0.052;
/// ...pushed fully this far apart (35 degrees)...
const PUSH_FULL: f32 = 0.61;
/// ...and never less than the stick's dead zone (XInput's recommended right-stick one, 8689).
const PUSH_DEAD_ZONE: f32 = 0.27;
/// The player's own stick turns past this much of its travel.
const STICK_TURNS: f32 = 0.25;
/// How old a stick read may be (ms), and the longest step of stick turning between two updates
/// (a stall must not spin the view).
const STICK_MAX_AGE_MS: u64 = 200;
const MAX_STEP_MS: u64 = 100;

/// The right stick that turns a camera `off` radians right of where the player looks (positive:
/// the look is left of it) toward the look: x right in -1..=1, 0 when close enough.
fn push_for(off: f32) -> f32 {
    if off.abs() < PUSH_FROM {
        return 0.0;
    }
    let amount = PUSH_DEAD_ZONE + (1.0 - PUSH_DEAD_ZONE) * ((off.abs() - PUSH_FROM) / (PUSH_FULL - PUSH_FROM)).min(1.0);
    -off.signum() * amount
}

struct State {
    character: usize,
    baked: BakedYaw,
    climb: Option<Climb>,
    /// Which way the view faces (radians in the world, positive left): head aim's own, turned only
    /// by the player's stick (climbing, by the stick's turn of the game camera). The view is this
    /// plus the head; the character is steered to it plus the head (or hand). Whatever else turns
    /// the character (a conversation facing the speaker, an attack's sway, a climb-up) turns the
    /// character and its camera but never the view: a view made from the game camera's direction
    /// moves with each of those, and they are too many to detect one by one.
    facing: Option<f32>,
    /// In a cutscene, the head's yaw as it began (radians): the cutscene's framing sits straight
    /// ahead of where the player faced then, and the head looks around in it.
    movie_head: Option<f32>,
    /// When the facing was last turned (`monaka_channel::tick`), for the stick's step.
    turned_at: u64,
}

static STATE: Mutex<State> =
    Mutex::new(State { character: 0, baked: BakedYaw::EMPTY, climb: None, facing: None, movie_head: None, turned_at: 0 });
static HEAD_AIM_TICK: AtomicU64 = AtomicU64::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static CLIMB_TRACES: AtomicU64 = AtomicU64::new(0);
/// How far the view backs off the wall now (metres) and which way (a horizontal world direction):
/// `climb_pullback` away from where the hands hold on ([`monaka_arms::traverse::wall_direction`]),
/// eased with the arms' blend into the climb.
pub fn climb_pullback() -> Option<(f32, [f32; 2])> {
    let distance = stereo::config().climb_pullback * monaka_arms::traverse::climb_share();
    let wall = monaka_arms::traverse::wall_direction()?;
    (distance > 1e-4).then(|| (distance, [-wall[0], -wall[1]]))
}
/// Whether head aim steered the character within the last half second (not in a cutscene).
pub fn live() -> bool {
    !MOVIE.load(Relaxed) && monaka_channel::tick().saturating_sub(HEAD_AIM_TICK.load(Relaxed)) < 500
}

/// Whether a cutscene has the player ([`on_movie`]).
pub fn cutscene() -> bool {
    MOVIE.load(Relaxed)
}

/// Whether the view is made with [`baked_yaw`]: once head aim knows the facing, and in a cutscene
/// (which may be under way when VR starts).
pub fn share_in_view() -> bool {
    FACING_KNOWN.load(Relaxed) || MOVIE.load(Relaxed)
}

/// Gives the right stick back to the game while head aim does not steer (a menu stops the
/// player's camera updates, a cutscene, a climb), and left trigger presses while it does not
/// steer at all: called every present.
pub fn keep_stick() {
    if !live() || monaka_arms::traverse::traversing() {
        monaka_pad::take_right_stick(false);
    }
    if !live() {
        TOOL.release();
    }
}

/// The camera's yaw (degrees, positive left) for a target yaw `t` is this minus `t` whenever the
/// camera follows the targets (in play and on ledges alike).
const CAMERA_YAW_OF_TARGET: f32 = -90.0;

/// A cutscene has the player: `IModelObject::IsObjectOnSomeMovie` on the character, asked at
/// every update, so a cutscene already under way when VR starts counts too. The engine's own state
/// is used because detecting cutscenes by how the camera behaves (off the look targets, the pitch
/// written not kept) catches them late, lets them go early, and takes conversations for them.
static MOVIE: AtomicBool = AtomicBool::new(false);
static MOVIE_SWITCHES: AtomicU64 = AtomicU64::new(0);
static FACING_KNOWN: AtomicBool = AtomicBool::new(false);
static ON_MOVIE: OnceLock<engine::IsOnMovieFn> = OnceLock::new();

/// Looks up `IModelObject::IsObjectOnSomeMovie`; head aim needs it.
pub fn resolve(engine_module: &Module) -> Result<(), Rejection> {
    let at = engine_module.export(engine::IS_OBJECT_ON_SOME_MOVIE).ok_or_else(|| Rejection::revision("the engine does not export IsObjectOnSomeMovie"))?;
    // SAFETY: the export has the signature its mangled name states, on x64.
    let _ = ON_MOVIE.set(unsafe { std::mem::transmute::<usize, engine::IsOnMovieFn>(at) });
    Ok(())
}

/// Whether a cutscene has `character` (a `PlayerDI`, checked by class: an `IModelObject` at its
/// start).
fn on_movie(character: usize) -> bool {
    let Some(whole) = complete_object(character) else { return false };
    // SAFETY: the engine's own query on the character's IModelObject, on the game thread inside
    // the game's camera update.
    ON_MOVIE.get().is_some_and(|f| unsafe { f(whole as *mut core::ffi::c_void) })
}

/// How many player camera updates head aim has steered.
pub fn updates() -> u64 {
    FRAMES.load(Relaxed)
}

/// The head yaw (radians) the game camera of the latest update carries.
pub fn baked_yaw() -> f32 {
    STATE.lock().map(|s| s.baked.lookup(FRAMES.load(Relaxed)).0).unwrap_or(0.0)
}

/// `IBaseCamera::FromForwardUpPos(forward, up, position)`.
pub unsafe extern "system" fn from_forward(camera: *mut core::ffi::c_void, forward: *const f32, up: *const f32, position: *const f32) {
    let _flight = InFlight::enter();
    let mut player = false;
    if stereo::capturing() {
        let site = caller();
        let config = stereo::config();
        crate::research::look::seen(camera as usize, forward as usize, site);
        if config.aim.head {
            let fields = &LOOK;
            player = Module::find(fields.player_call.0).is_some_and(|m| site == m.at(fields.player_call.1));
            steer(fields, camera as usize, site, forward as usize);
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { FROM_FORWARD_ORIGINAL.get()(camera, forward, up, position) };
    if player {
        // The player camera is built: turned to the view before the game lays out its HUD.
        stereo::turn_after_game_update();
    }
}

fn steer(fields: &LookFields, camera: usize, site: usize, forward: usize) {
    let Some(module) = Module::find(fields.player_call.0) else { return };
    if site != module.at(fields.player_call.1) {
        return;
    }
    let Some(character) = (fields.character)(camera) else { return };
    stereo::set_player_camera(camera);
    let pose = stereo::pending_pose();
    if pose.valid == 0 {
        return;
    }
    let (Some(yaw), Some(pitch)) = (mem::read::<f32>(character + fields.yaw), mem::read::<f32>(character + fields.pitch)) else { return };
    // The camera's argument is its backward axis (DL2 and DL1): look = -argument; its yaw is the
    // levelled camera's.
    let Some(camera_yaw) = mem::read::<[f32; 3]>(forward).map(|f| f[0].atan2(f[2])).filter(|y| y.is_finite()) else { return };
    // Hand aim: the controller's direction replaces the head's ([`monaka_arms::aim`]). With the
    // hand put on the controller (`hand_rig`) the weapon sits in the palm, a forearm from the
    // camera the game shoots from: the camera aims at the point the weapon points at
    // `WEAPON_CONVERGENCE` metres out, where the viewer's reticle sits too. The tracking origin
    // is the view's (`stereo::tracking_origin`), so that point is also the direction. Without the
    // rig the controller's aim pose stands in for the palm. Melee (by choice) and climbing aim
    // with the head; the left hand's tool aims with the left controller, whatever the right hand
    // holds.
    let config = stereo::config();
    let settings = config.arms_aim;
    let now = monaka_channel::tick();
    let aim = settings.aim_at(now, pose.orientation, |side| if config.aim.rig { stereo::pending_palm_of(side) } else { stereo::pending_hand_of(side) });
    monaka_arms::aim::note_source(aim.by_hand, config.aim.hand);
    let (aim_yaw, aim_pitch, tool) = (aim.yaw, aim.pitch, aim.tool);
    let head_yaw = head_yaw_pitch(pose.orientation).0;
    let Ok(mut state) = STATE.lock() else { return };
    // This update's camera was built from the targets the previous update wrote.
    let frame = FRAMES.fetch_add(1, Relaxed) + 1;
    let step = now.saturating_sub(state.turned_at).min(MAX_STEP_MS) as f32 / 1000.0;
    state.turned_at = now;
    if state.character != character {
        // A new character (a load, a respawn): the view faces where its camera does.
        state.character = character;
        state.facing = None;
        state.climb = None;
    }
    let movie = on_movie(character);
    if movie != MOVIE.swap(movie, Relaxed) {
        if movie {
            state.movie_head = Some(head_yaw);
        } else if let Some(began) = state.movie_head.take() {
            // The view goes on from the cutscene's last framing.
            state.facing = Some(wrapped(camera_yaw - began));
        }
        if MOVIE_SWITCHES.fetch_add(1, Relaxed) < 200 {
            log!(
                "head aim: {} (the camera looks {:.1} degrees)",
                if movie { "a cutscene has the player; the view is its camera plus the head" } else { "the cutscene let go; steering" },
                camera_yaw.to_degrees()
            );
        }
    }
    crate::research::look::update(frame, yaw, pitch, camera_yaw.to_degrees(), state.facing.map(f32::to_degrees), head_yaw.to_degrees(), movie);
    // A left trigger press waits for the left controller's aim; climbing and in a cutscene nothing
    // waits ([`monaka_arms::aim::ToolAim`]).
    TOOL.gate(!movie && TOOL.possible(&settings), tool, frame);
    if movie {
        // Nothing is written; the view is the cutscene's camera levelled, the head's turn as it
        // began kept out, plus the head.
        let began = *state.movie_head.get_or_insert(head_yaw);
        state.baked.record(frame, began);
        return;
    }
    let mut facing = *state.facing.get_or_insert(camera_yaw);
    FACING_KNOWN.store(true, Relaxed);
    match crate::player::spots::take_command(now) {
        Some(crate::player::spots::Command::Save(name)) => crate::player::spots::save(&name, character, facing),
        Some(crate::player::spots::Command::Go(name)) => {
            if let Some(spot) = crate::player::spots::go(&name, character) {
                facing = spot;
            }
        }
        None => {}
    }
    // Climbing or hanging, the game turns the character by where it looks (letting go with one
    // hand to reach back, taking hold again) and fights look angles written into it: those kick
    // the character back. The stick turns it there cleanly, so the head goes in as the stick: it
    // pushes the right stick until the game's camera looks where the head does. The player's own
    // stick turns the facing by what it turns the game's camera.
    if monaka_arms::traverse::traversing() {
        monaka_pad::take_right_stick(false);
        let mut climb = state.climb.unwrap_or_else(|| {
            log!("head aim: climbing; the head turns the character through the right stick");
            Climb { camera: camera_yaw, push: 0.0, traced: 0 }
        });
        let turned = wrapped(camera_yaw - climb.camera);
        climb.camera = camera_yaw;
        let thumb = monaka_pad::right_stick(100).map_or(0.0, |s| s[0] - climb.push);
        let off = wrapped(facing + aim_yaw - camera_yaw);
        if thumb.abs() > STICK_TURNS {
            // The player turns: the facing goes with the game camera, head aim lets go of the stick.
            facing = wrapped(facing + turned);
            climb.push = 0.0;
        } else {
            climb.push = push_for(off);
        }
        monaka_pad::press(monaka_core::pad::Gamepad { thumb_rx: (climb.push * 32767.0) as i16, ..Default::default() }, 100);
        state.facing = Some(facing);
        state.baked.record(frame, wrapped(camera_yaw - facing));
        // Still live: the arms and the vertical look input go on as while steering.
        HEAD_AIM_TICK.store(now, Relaxed);
        if now.saturating_sub(climb.traced) >= 100 && CLIMB_TRACES.fetch_add(1, Relaxed) < 1500 {
            climb.traced = now;
            log!(
                "  climb look: camera yaw {:.1}, the head looks {:.1} (facing {:.1} + head {:.1}): {:.1} apart, stick pushed {:.2}, the player's {thumb:.2}",
                camera_yaw.to_degrees(),
                (facing + aim_yaw).to_degrees(),
                facing.to_degrees(),
                aim_yaw.to_degrees(),
                off.to_degrees(),
                climb.push
            );
        }
        state.climb = Some(climb);
        return;
    }
    if state.climb.take().is_some() {
        monaka_pad::press(monaka_core::pad::Gamepad::default(), 0);
        log!("head aim: the climb ended");
    }
    // The right stick turns the facing; the game reads it centred.
    monaka_pad::take_right_stick(true);
    if let Some([x, _]) = monaka_pad::right_stick(STICK_MAX_AGE_MS) {
        facing = wrapped(facing - monaka_core::pad::turn_response(x) * config.stick_speed[0].to_radians() * step);
    }
    state.facing = Some(facing);
    // What the camera carries beyond the facing: turned back out, the view is the facing plus the
    // head, whatever turned the camera.
    state.baked.record(frame, wrapped(camera_yaw - facing));
    // The character looks where the facing plus the head (or hand) does: kept nearest the yaw it
    // has, so a player turned round in the room does not send it a full turn off (the game would
    // turn the character all the way round to it).
    let target = yaw + wrapped_degrees(CAMERA_YAW_OF_TARGET - wrapped(facing + aim_yaw).to_degrees() - yaw);
    let limit = fields.convention.pitch_limit;
    let target_pitch = (aim_pitch * fields.convention.pitch_per_radian).clamp(-limit, limit);
    mem::write(character + fields.yaw, target);
    mem::write(character + fields.pitch, target_pitch);
    HEAD_AIM_TICK.store(now, Relaxed);
    crate::research::look::steered(&crate::research::look::Steer {
        frame,
        camera_yaw: camera_yaw.to_degrees(),
        facing: facing.to_degrees(),
        aim_yaw: aim_yaw.to_degrees(),
        aim_pitch: aim_pitch.to_degrees(),
        yaw,
        target,
        pitch,
        target_pitch,
        character,
    });
}

/// Hands the character a look without the head (end of a run): the facing, level.
pub fn release() {
    monaka_pad::take_right_stick(false);
    TOOL.release();
    let fields = &LOOK;
    let Ok(state) = STATE.lock() else { return };
    let (character, Some(facing)) = (state.character, state.facing) else { return };
    if MOVIE.load(Relaxed) {
        return;
    }
    let Some(yaw) = mem::read::<f32>(character + fields.yaw) else { return };
    mem::write(character + fields.yaw, yaw + wrapped_degrees(CAMERA_YAW_OF_TARGET - facing.to_degrees() - yaw));
    mem::write(character + fields.pitch, 0.0f32);
}

/// Logs the vertical look inputs neutralised (end of a run).
pub fn report() {
    log!("vertical look inputs neutralised: {}", PITCH_INPUTS_BLOCKED.load(Relaxed));
    monaka_arms::aim::report_sources(stereo::config().aim.hand);
}
