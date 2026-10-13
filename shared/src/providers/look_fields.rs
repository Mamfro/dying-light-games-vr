//! Provider: Chrome's look fields, which the look's targets delivery reads and writes
//! (`monaka_body::look::targets`, Chrome's head aim [`HeadAim`] around it): where a game keeps the
//! character's look targets and the character ([`Fields`]), the player character behind the arms
//! visual's camera target ([`HeadAim::character_of`]), the game's player camera update seen at its
//! camera setter ([`PlayerUpdate`], [`HeadAim::player_update`]), the engine's cutscene query
//! ([`Cutscene`]), and the game's input-action converter with the build's vertical look actions
//! ([`input_action`], [`pitch_actions_match`]).

use monaka_body::look::aim::neutral_pitch;
use monaka_body::look::targets::{self, LookFields, Steered};
use monaka_body::look::{self, StandDown};
use monaka_core::aim::Convention;
use monaka_core::camera::{Mat34, from_back_up};
use monaka_core::protocol::HeadPose;
use monaka_core::sync::Locked;
use monaka_hook::module::Module;
use monaka_hook::{InFlight, Original, mem};
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::*};

/// Where a game keeps the look and the character (RVAs in the game DLL, offsets in objects).
#[derive(Clone, Copy, Debug)]
pub struct Fields {
    /// The character's look targets, degrees, yaw turning right as it grows.
    pub target_yaw: usize,
    pub target_pitch: usize,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
    /// The arms visual's `ICameraTarget` base's vtable (`PlayerFppVis_PH`); the character sits
    /// `target_character` before that base.
    pub vis_vtable: usize,
    pub target_character: usize,
}

/// The game's player camera update setting its camera, once a frame: Dying Light 2 seen at the
/// engine's `IBaseCamera::FromForwardUpPos`, The Beast at the renderer's `CCamera::SetView(vec3)`
/// it jumps on to (the same return address, the same vectors). The setter's "forward" is the
/// camera's back axis.
#[derive(Clone, Copy, Debug)]
pub struct PlayerUpdate {
    pub back: [f32; 3],
    pub up: [f32; 3],
    pub position: [f32; 3],
    /// The camera-to-world the vectors make (none for degenerate ones).
    pub matrix: Option<Mat34>,
}

impl PlayerUpdate {
    /// The camera's pitch (radians, positive up), none looking straight up or down.
    pub fn pitch(&self) -> Option<f32> {
        let back = self.back;
        let horizontal = (back[0] * back[0] + back[2] * back[2]).sqrt();
        (horizontal > 1e-4).then(|| -back[1].atan2(horizontal))
    }
}

/// How a build's code loads a vertical look action's id beside its name, for
/// [`pitch_actions_match`].
#[derive(Clone, Copy, Debug)]
pub enum IdLoad {
    /// `mov dword [rip+disp32], imm32` (`48 c7 05 disp32 imm32`): Dying Light 2.
    MemImm32,
    /// `mov edx, imm32` (`ba imm32`): The Beast.
    EdxImm32,
}

/// The engine's query whether a cutscene has an object: `IModelObject::IsObjectOnSomeMovie`.
pub const IS_OBJECT_ON_SOME_MOVIE: &str = "?IsObjectOnSomeMovie@IModelObject@@QEAA_NXZ";
type IsOnMovieFn = unsafe extern "system" fn(usize) -> bool;

/// How long a cutscene reported at one update holds (ms): the updates renew it.
const CUTSCENE_FRESH_MS: u64 = 500;

/// Whether a cutscene has the player, by the engine's own state ([`IS_OBJECT_ON_SOME_MOVIE`] on
/// the character), asked at every player camera update, so a cutscene already under way when VR
/// starts counts too. The engine's own state is used because detecting cutscenes by how the camera
/// behaves (off the look targets, the pitch written not kept) catches them late, lets them go
/// early, and takes conversations for them. Each answer goes into the run's situations
/// (`Situation::CameraTaken`). A build whose engine does not export the query has none: nothing is
/// asked and nothing reported.
pub struct Cutscene {
    query: OnceLock<IsOnMovieFn>,
    on: AtomicBool,
}

/// One answer of [`Cutscene::ask`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asked {
    /// A cutscene has the player now.
    pub cutscene: bool,
    /// The answer differs from the one before.
    pub changed: bool,
}

impl Cutscene {
    pub const fn new() -> Self {
        Self { query: OnceLock::new(), on: AtomicBool::new(false) }
    }

    /// Looks the query up in `engine`; why not, when it does not export it.
    pub fn resolve(&self, engine: &Module) -> Result<(), String> {
        let at = engine.export(IS_OBJECT_ON_SOME_MOVIE).ok_or("the engine does not export IsObjectOnSomeMovie")?;
        // SAFETY: the export has the signature its mangled name states, on x64.
        let _ = self.query.set(unsafe { std::mem::transmute::<usize, IsOnMovieFn>(at) });
        Ok(())
    }

    /// Whether the query was found.
    pub fn resolved(&self) -> bool {
        self.query.get().is_some()
    }

    /// Asks whether a cutscene has `character` (the player character, an `IModelObject` at the start
    /// of its complete object), reported as `Situation::CameraTaken`. On the game thread, inside the
    /// game's player camera update. Never a cutscene without the query.
    pub fn ask(&self, character: usize) -> Asked {
        let Some(query) = self.query.get() else { return Asked { cutscene: false, changed: false } };
        let whole = monaka_hook::probe::complete_object(character);
        // SAFETY: the engine's own query on the character's IModelObject, on the game thread inside
        // the game's camera update.
        let cutscene = whole.is_some_and(|whole| unsafe { query(whole) });
        monaka_producer::situation::report(monaka_producer::situation::Situation::CameraTaken, cutscene, CUTSCENE_FRESH_MS);
        Asked { cutscene, changed: self.on.swap(cutscene, Relaxed) != cutscene }
    }

    /// The latest answer.
    pub fn on(&self) -> bool {
        self.on.load(Relaxed)
    }
}

impl Default for Cutscene {
    fn default() -> Self {
        Self::new()
    }
}

/// Head aim on a Chrome Engine character: the look's steering of its look targets
/// (`monaka_body::look::targets`) with what the Chrome Engine adds: where the character and its
/// targets are ([`Fields`]), the game DLL its classes are in, the player camera its latest update
/// set, the engine's cutscene query, the view bob held while it steers and the throw or bow by hand
/// that aims itself.
pub struct HeadAim {
    pub(crate) fields: Fields,
    pub(crate) gamedll: AtomicUsize,
    /// The player camera as its latest update set it (camera-to-world, the game's own), keyed by
    /// the camera object the game set it on.
    pub(crate) player_camera: Mutex<(usize, Mat34)>,
    /// A cutscene has the player: head aim stands down ([`HeadAim::steer`]).
    cutscene: Cutscene,
    /// The game camera's own pitch (radians, positive up, f32 bits), for the pitch lock
    /// ([`HeadAim::game_pitch`]); whether it is known, and whether a player camera update gave it.
    pitch: AtomicU32,
    pitch_known: AtomicBool,
    pitch_from_update: AtomicBool,
}

impl HeadAim {
    pub const fn new(fields: Fields) -> Self {
        Self {
            fields,
            gamedll: AtomicUsize::new(0),
            player_camera: Mutex::new((0, [0.0; 12])),
            cutscene: Cutscene::new(),
            pitch: AtomicU32::new(0),
            pitch_known: AtomicBool::new(false),
            pitch_from_update: AtomicBool::new(false),
        }
    }

    /// The game camera's own pitch (radians, positive up), when known: the character's own look
    /// while head aim steers (the camera then carries the head's pitch too), else the camera's as
    /// the player camera update set it ([`HeadAim::note_camera_pitch`]).
    pub fn game_pitch(&self) -> Option<f32> {
        self.pitch_known.load(Acquire).then(|| f32::from_bits(self.pitch.load(Relaxed)))
    }

    fn note_pitch(&self, pitch: f32, from_update: bool) {
        self.pitch.store(pitch.to_bits(), Relaxed);
        self.pitch_known.store(true, Release);
        if from_update {
            self.pitch_from_update.store(true, Release);
        }
    }

    /// A player camera update head aim did not steer set the camera at `pitch` (radians, up).
    pub fn note_camera_pitch(&self, pitch: f32) {
        self.note_pitch(pitch, true);
    }

    /// A scene was drawn from a camera at `pitch` (radians, up): the game's pitch until a player
    /// camera update gives it.
    pub fn note_scene_pitch(&self, pitch: f32) {
        if !self.pitch_from_update.load(Acquire) {
            self.note_pitch(pitch, false);
        }
    }

    /// Where the character's look targets are, for the look's steering.
    pub fn look_fields(&self) -> LookFields {
        LookFields { yaw: self.fields.target_yaw, pitch: self.fields.target_pitch, convention: Convention::DEGREES_YAW_RIGHT }
    }

    /// The cutscene detector head aim stands down by (resolved at start where the engine has it).
    pub fn cutscene(&self) -> &Cutscene {
        &self.cutscene
    }

    /// At start: the game DLL the vtables are in (0: no character is recognised).
    pub fn set_gamedll(&self, gamedll: usize) {
        self.gamedll.store(gamedll, Release);
    }

    /// Whether head aim steered the character within the last half second.
    pub fn live(&self) -> bool {
        look::live()
    }

    /// One player camera update ([`targets::steer`]): steers `character` toward the head (or the
    /// aiming controller), standing down while a cutscene has the player (the engine's own query,
    /// [`Cutscene::ask`]); a throw or a bow by hand aims itself (`crate::providers::fpp::head_aims`);
    /// the view held level while it steers (`crate::providers::view_bob::hold`).
    pub fn steer(&self, character: usize, head: Option<HeadPose>, counter: Option<u64>) -> Option<Steered> {
        let asked = self.cutscene.ask(character);
        let stand_down = StandDown { now: asked.cutscene, changed: asked.changed };
        let steered = targets::steer(self.look_fields(), character, head, counter, stand_down, crate::providers::fpp::head_aims);
        crate::providers::view_bob::hold(character, steered.is_some());
        if let Some(steered) = &steered {
            self.note_pitch(steered.own.pitch.to_radians(), true);
        }
        steered
    }

    /// The head yaw (radians) baked into the game camera of frame `counter`.
    pub fn baked_at(&self, counter: Option<u64>) -> f32 {
        targets::baked_at(counter)
    }

    /// The tracking origin the eyes and arms are built on ([`targets::tracking_origin`]).
    pub fn tracking_origin(&self, game: &Mat34, counter: Option<u64>) -> Option<Mat34> {
        targets::tracking_origin(game, counter)
    }

    /// Vertical look inputs neutralised so far.
    pub fn pitch_blocked(&self) -> u64 {
        look::pitch_blocked()
    }

    pub fn fields(&self) -> &Fields {
        &self.fields
    }

    /// Whether `target` is the arms visual's camera target (by vtable).
    pub fn is_arms_target(&self, target: usize) -> bool {
        let gamedll = self.gamedll.load(Acquire);
        gamedll != 0 && mem::read::<usize>(target) == Some(gamedll + self.fields.vis_vtable)
    }

    /// The player character behind the arms visual's camera target, both checked by vtable.
    pub fn character_of(&self, target: usize) -> Option<usize> {
        let gamedll = self.gamedll.load(Acquire);
        if !self.is_arms_target(target) {
            return None;
        }
        let character = mem::read::<usize>(target.wrapping_sub(self.fields.target_character)).filter(|&c| c != 0)?;
        (mem::read::<usize>(character)? == gamedll + self.fields.player_vtable).then_some(character)
    }

    /// A call of the player camera's setter on `camera` with the vectors at `back`, `up` and
    /// `position`, from `caller` (its return address): when that is the player camera update's
    /// (`update`, absolute; 0 while unknown), the vectors read and the camera noted under `camera`
    /// for the arms ([`HeadAim::player_camera`]). `None` for any other call.
    pub fn player_update(&self, update: usize, caller: usize, camera: usize, back: usize, up: usize, position: usize) -> Option<PlayerUpdate> {
        if update == 0 || caller != update {
            return None;
        }
        let (back, up, position) = (mem::read::<[f32; 3]>(back)?, mem::read::<[f32; 3]>(up)?, mem::read::<[f32; 3]>(position)?);
        let matrix = from_back_up(back, up, position);
        if let Some(matrix) = matrix {
            *self.player_camera.locked() = (camera, matrix);
        }
        Some(PlayerUpdate { back, up, position, matrix })
    }

    /// Camera `key`'s latest camera-to-world matrix, if it was the player camera's latest update.
    pub fn player_camera(&self, key: usize) -> Option<Mat34> {
        let last = self.player_camera.lock().ok()?;
        (last.0 == key && key != 0).then_some(last.1)
    }
}

/// The game's input-action converter `(binding, receivers, value, source, repeat)`: action id at
/// binding+0, flags at +0x10 (0x10: inverted, neutral 1).
pub type InputActionFn = unsafe extern "system" fn(binding: usize, receivers: usize, value: f32, source: bool, repeat: bool);

pub static INPUT_ACTION: Original<InputActionFn> = Original::new();
/// A build's vertical look actions: the RVA of each id's load, its name's RVA, the id, the name.
pub type PitchActions = [(usize, usize, u32, &'static str)];
/// Head aim and its vertical look actions, once the converter is hooked.
static CONVERTER: OnceLock<(&'static HeadAim, &'static PitchActions)> = OnceLock::new();

/// The converter is hooked for `aim`, its vertical look actions in `table`.
pub fn convert_for(aim: &'static HeadAim, table: &'static PitchActions) {
    let _ = CONVERTER.set((aim, table));
}

/// The game's input-action converter: while head aim steers the character, the head alone pitches
/// it, so vertical look input (mouse and stick) gets its neutral value; otherwise the pitch lock
/// passes it only back toward level (`monaka_body::pitch_lock`, when on).
///
/// # Safety
/// Only as the converter's detour ([`INPUT_ACTION`] its original), with the game's own arguments.
pub unsafe extern "system" fn input_action(binding: usize, receivers: usize, value: f32, source: bool, repeat: bool) {
    let _flight = InFlight::enter();
    let value = match CONVERTER.get() {
        Some((aim, table)) if aim.live() => look::neutral_pitch_if_live(binding, value, |action| is_pitch_action(table, action)),
        Some((_, table)) => match neutral_pitch(binding, |action| is_pitch_action(table, action)) {
            Some(neutral) => monaka_body::pitch_lock::pass(value, neutral, turns_up(table, mem::read::<u32>(binding).unwrap_or(0))),
            None => value,
        },
        None => value,
    };
    // SAFETY: forwards the game's own call, the value possibly neutralised.
    unsafe { INPUT_ACTION.get()(binding, receivers, value, source, repeat) }
}

/// Whether `action` is one of `table`'s vertical look actions (RVA of the id's load, name RVA,
/// id, name).
pub fn is_pitch_action(table: &[(usize, usize, u32, &str)], action: u32) -> bool {
    table.iter().any(|&(_, _, id, _)| id == action)
}

/// Whether `action` turns the camera up: `table`'s first and third actions (look up, rotate up).
pub fn turns_up(table: &[(usize, usize, u32, &str)], action: u32) -> bool {
    [0, 2].into_iter().any(|i| table.get(i).is_some_and(|&(_, _, id, _)| id == action))
}

/// Checks the vertical look actions against the engine's action table: each name where the build
/// keeps it, and its id loaded beside it the way `load` says.
pub fn pitch_actions_match(engine: &Module, table: &[(usize, usize, u32, &str)], load: IdLoad) -> Result<(), String> {
    for &(load_at, name_at, id, name) in table {
        let found = mem::read_c_string(engine.at(name_at), 64);
        let loads_id = match load {
            IdLoad::MemImm32 => mem::read::<[u8; 11]>(engine.at(load_at)).is_some_and(|c| c[..3] == [0x48, 0xc7, 0x05] && u32::from_le_bytes([c[7], c[8], c[9], c[10]]) == id),
            IdLoad::EdxImm32 => {
                let mut expected = [0xba, 0, 0, 0, 0];
                expected[1..].copy_from_slice(&id.to_le_bytes());
                engine.bytes_match(load_at, &expected)
            }
        };
        if found.as_deref() != Some(name) || !loads_id {
            return Err(format!("action {name} (id {id:#x}) is not where it was inspected ({found:?})"));
        }
    }
    Ok(())
}
