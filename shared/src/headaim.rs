//! Head aim on Chrome Engine's player character (Dying Light 2, The Beast): the character's own
//! look targets follow the head, so it looks, aims and attacks where the head points. On each
//! player camera update the target pitch becomes the head's and the head's yaw is added to the
//! target yaw (minus what was added before), so mouse and stick turning keep accumulating on top;
//! vertical look input is neutralised meanwhile ([`HeadAim::neutral_pitch_if_live`]). The camera
//! an update sets was built from the previous update's targets: the eyes are built on it levelled,
//! with that head yaw turned back out ([`HeadAim::turn_out`], recorded per game frame). What was
//! added is taken out again when VR stops ([`HeadAim::release`]).
//!
//! Hand aim: the aiming controller's direction replaces the head's; with a melee weapon or bare
//! hands drawn and the hand rig on, the head aims; the left hand's tool aims with the left
//! controller (`monaka_arms::aim`). A game supplies its offsets ([`Fields`]) and where it steers
//! from (DL2 at the player camera setter, The Beast at the arms callback); the state, the steps
//! and the tallies are here.

use monaka_arms::aim::{AimSettings, LookFields, LookSteer, TOOL, neutral_pitch, note_source};
use monaka_core::aim::{BakedYaw, Convention, Look};
use monaka_core::camera::{Mat34, from_back_up, level, turn};
use monaka_core::protocol::HeadPose;
use monaka_hook::mem;
use monaka_hook::module::Module;
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};

/// How long after an update head aim counts as steering (ms): a pause, a loading screen or a
/// cutscene stops the updates, and the head's share is then left in place.
const LIVE_MS: u64 = 500;

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

/// What a steering update did.
#[derive(Clone, Copy, Debug)]
pub struct Steered {
    /// The look written and the character's own (what it had without the head), degrees.
    pub written: Look,
    pub own: Look,
    /// The head yaw (radians) the camera this update set carries (the previous update's share).
    pub baked: f32,
    /// The update's number this run.
    pub update: u64,
    pub by_hand: bool,
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

pub struct HeadAim {
    fields: Fields,
    steer: Mutex<LookSteer>,
    settings: Mutex<Option<AimSettings>>,
    gamedll: AtomicUsize,
    /// While set, the next update hands the head's share back.
    releasing: AtomicBool,
    /// When head aim last steered the character (`monaka_channel::tick`).
    tick: AtomicU64,
    updates: AtomicU64,
    pitch_blocked: AtomicU64,
    baked_misses: AtomicU64,
    /// The head yaw (radians) baked into the game camera of each game frame, by game counter,
    /// and the latest one (f32 bits) for games that number no frames.
    baked: Mutex<BakedYaw>,
    latest_baked: AtomicU32,
    /// The player camera as its latest update set it (camera-to-world, the game's own), keyed by
    /// the camera object the game set it on.
    player_camera: Mutex<(usize, Mat34)>,
}

impl HeadAim {
    pub const fn new(fields: Fields) -> Self {
        Self {
            fields,
            steer: Mutex::new(LookSteer::new(LookFields { yaw: fields.target_yaw, pitch: fields.target_pitch, convention: Convention::DEGREES_YAW_RIGHT })),
            settings: Mutex::new(None),
            gamedll: AtomicUsize::new(0),
            releasing: AtomicBool::new(false),
            tick: AtomicU64::new(0),
            updates: AtomicU64::new(0),
            pitch_blocked: AtomicU64::new(0),
            baked_misses: AtomicU64::new(0),
            baked: Mutex::new(BakedYaw::EMPTY),
            latest_baked: AtomicU32::new(0),
            player_camera: Mutex::new((0, [0.0; 12])),
        }
    }

    /// At start: what aims, and the game DLL the vtables are in (0: no character is recognised).
    pub fn configure(&self, settings: AimSettings, gamedll: usize) {
        *self.settings.lock().unwrap_or_else(|e| e.into_inner()) = Some(settings);
        self.gamedll.store(gamedll, Release);
        self.releasing.store(false, Release);
    }

    /// What aims this run (nothing until configured).
    pub fn settings(&self) -> AimSettings {
        self.settings.lock().ok().and_then(|s| *s).unwrap_or(AimSettings { head_aim: false, hand_aim: false, aim_hand: 1, hand_rig: false, melee_aim_head: true, tool_aim: false })
    }

    pub fn fields(&self) -> &Fields {
        &self.fields
    }

    /// Whether head aim steered the character within the last half second.
    pub fn live(&self) -> bool {
        monaka_channel::tick().saturating_sub(self.tick.load(Acquire)) < LIVE_MS
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

    /// The head yaw (radians) the targets carry for `character` now.
    pub fn share(&self, character: usize) -> f32 {
        self.steer.lock().map(|s| s.share(character)).unwrap_or(0.0)
    }

    /// The share the latest update's camera carries, while head aim steers.
    pub fn current_share(&self) -> Option<f32> {
        let steer = self.steer.lock().ok()?;
        (self.live() && steer.holds_share()).then(|| steer.share(steer.applied.owner()))
    }

    /// One player camera update: steers `character` for the next update toward the head (or the
    /// aiming controller), recording the share this update's camera carries under `counter` (the
    /// game frame, when the game numbers them). With no `head` (VR stopped, no pose) or while
    /// releasing, hands the game's own angles and the left trigger back; `None` then.
    pub fn steer(&self, character: usize, head: Option<HeadPose>, counter: Option<u64>) -> Option<Steered> {
        let settings = self.settings();
        let mut steer = self.steer.lock().unwrap_or_else(|e| e.into_inner());
        // The share this update's camera carries (written by the previous update).
        let baked = steer.share(character);
        self.latest_baked.store(baked.to_bits(), Relaxed);
        if let Some(counter) = counter {
            self.baked.lock().unwrap_or_else(|e| e.into_inner()).record(counter, baked);
        }
        let head = head.filter(|h| h.valid != 0 && settings.head_aim && !self.releasing.load(Acquire));
        if let Some(head) = head {
            let aim = settings.aim_at(monaka_channel::tick(), head.orientation, monaka_arms::controllers::palm);
            note_source(aim.by_hand, settings.hand_aim);
            match steer.steer(character, aim.yaw, aim.pitch) {
                Some((written, own)) => {
                    let update = self.updates.fetch_add(1, Relaxed);
                    // A left trigger press waits for the left controller's aim (no cutscene state here).
                    TOOL.gate(TOOL.possible(&settings), aim.tool, update);
                    self.tick.store(monaka_channel::tick(), Release);
                    if update == 0 {
                        log!("head aim: steering the character ({} aim; target look {:?})", if aim.by_hand { "hand" } else { "head" }, steer.fields.read(character));
                    }
                    return Some(Steered { written, own, baked, update, by_hand: aim.by_hand });
                }
                None => monaka_producer::log_first!(3, "head aim: not steering: look {:?}, head yaw {:.2} pitch {:.2}", steer.fields.read(character), aim.yaw, aim.pitch),
            }
        }
        TOOL.release();
        steer.release(character);
        None
    }

    /// The head yaw (radians) baked into the game camera of frame `counter` (the latest known
    /// when the frame is not on record, or with no counter).
    pub fn baked_at(&self, counter: Option<u64>) -> f32 {
        match counter {
            Some(counter) => {
                let (yaw, found) = self.baked.lock().unwrap_or_else(|e| e.into_inner()).lookup(counter);
                if !found {
                    self.baked_misses.fetch_add(1, Relaxed);
                }
                yaw
            }
            None => f32::from_bits(self.latest_baked.load(Relaxed)),
        }
    }

    /// Turns a levelled game camera back by the head yaw baked into frame `counter`, so a head
    /// turn is not counted twice (nothing while head aim is not steering).
    pub fn turn_out(&self, view: &mut Mat34, counter: Option<u64>) {
        if self.live() {
            *view = turn(view, -self.baked_at(counter));
        }
    }

    /// The tracking origin the eyes and arms are built on: the game camera levelled, the head yaw
    /// baked into frame `counter` turned back out, and moved by room-scale following
    /// ([`monaka_arms::roomscale::shift_origin`]).
    pub fn tracking_origin(&self, game: &Mat34, counter: Option<u64>) -> Option<Mat34> {
        let mut origin = level(game)?;
        self.turn_out(&mut origin, counter);
        monaka_arms::roomscale::shift_origin(&mut origin);
        Some(origin)
    }

    /// The player camera update set camera `key` from these vectors (the setter's "forward" is
    /// the back axis): remembered for the arms, and returned for a game's own probes.
    pub fn note_player_camera(&self, key: usize, back: [f32; 3], up: [f32; 3], position: [f32; 3]) -> Option<Mat34> {
        let matrix = from_back_up(back, up, position)?;
        *self.player_camera.lock().unwrap_or_else(|e| e.into_inner()) = (key, matrix);
        Some(matrix)
    }

    /// Camera `key`'s latest camera-to-world matrix, if it was the player camera's latest update.
    pub fn player_camera(&self, key: usize) -> Option<Mat34> {
        let last = self.player_camera.lock().ok()?;
        (last.0 == key && key != 0).then_some(last.1)
    }

    /// Hands the head's share back at the next update (waits up to `timeout_ms` for it) and logs
    /// the run's tally.
    pub fn release(&self, timeout_ms: u64) {
        let settings = self.settings();
        self.releasing.store(true, Release);
        TOOL.release();
        if settings.head_aim && !monaka_producer::wait_until(timeout_ms, || !self.steer.lock().map(|s| s.holds_share()).unwrap_or(false)) {
            log!("head aim: the head's share left in the character (no update during stop)");
        }
        log!(
            "head aim: {} updates; vertical look inputs neutralised {}; baked yaw not found by game counter {} times",
            self.updates.load(Relaxed),
            self.pitch_blocked.load(Relaxed),
            self.baked_misses.load(Relaxed)
        );
        monaka_arms::aim::report_sources(settings.hand_aim);
    }

    /// The game's input-action converter got `value` for the action at `binding`: while head aim
    /// steers, a vertical look action (`is_pitch`) gets its neutral value (the head alone pitches
    /// the character); the value as given otherwise.
    pub fn neutral_pitch_if_live(&self, binding: usize, value: f32, is_pitch: impl Fn(u32) -> bool) -> f32 {
        if self.live()
            && let Some(neutral) = neutral_pitch(binding, is_pitch)
        {
            if value != neutral {
                self.pitch_blocked.fetch_add(1, Relaxed);
            }
            return neutral;
        }
        value
    }

    /// Vertical look inputs neutralised so far.
    pub fn pitch_blocked(&self) -> u64 {
        self.pitch_blocked.load(Relaxed)
    }
}

/// Whether `action` is one of `table`'s vertical look actions (RVA of the id's load, name RVA,
/// id, name).
pub fn is_pitch_action(table: &[(usize, usize, u32, &str)], action: u32) -> bool {
    table.iter().any(|&(_, _, id, _)| id == action)
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

