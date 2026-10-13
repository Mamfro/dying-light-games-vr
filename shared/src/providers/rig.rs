//! Provider: the skeleton's posing moment and the rig glue (the arms callback's hook, the pose the
//! rig goes on), for the hand rig (`monaka_body::hand_rig`).
//!
//! The first-person arms on the controllers (`monaka_body`) in Dying Light 2 and The Beast (Dying
//! Light 1, whose aim is its own, calls [`crate::providers::fpp::around`] itself): the player
//! camera update calls the arms visual's camera-target callback right after setting the camera;
//! around it the arms are snapshot as animated, then put on the controllers, and melee swings
//! attack ([`crate::providers::fpp::around`]). What both games do around that call is here: the
//! vtable check, the options, the pose the rig goes on (the game's own player camera as head aim
//! noted it, the tracking origin with head aim's share turned out and the head model's shift in,
//! the head, the controllers), and the callback's hook and detour themselves ([`hook`]). A game
//! supplies its [`Arms`]: its [`Facts`] and, per call, whether VR runs, the head and the game frame
//! ([`Moment`]).

use crate::feature::Chrome;
use crate::providers::fpp::{self, CameraTargetFn, Layout};
use crate::providers::look_fields::HeadAim;
use monaka_body::hand_rig::{Hands, Palms};
use monaka_body::{controllers, melee};
use monaka_core::camera::{HeadPosition, Mat34};
use monaka_core::protocol::{HeadPose, LEFT_HAND};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::Rejection;
use std::sync::Mutex;
use monaka_core::sync::Locked;

/// Where a game keeps its arms (RVAs in the game DLL, offsets in objects).
#[derive(Clone, Copy, Debug)]
pub struct Facts {
    pub callback: fpp::Callback,
    /// The `ICameraTarget` base within the arms visual.
    pub icamera_target: usize,
    pub skeleton: crate::providers::coskeleton::Layout,
    pub arms: Layout,
    /// The key head aim notes the player camera under, from the camera the callback gets: the
    /// camera itself, or the object at this offset in it (The Beast's renderer camera).
    pub camera_key_at: Option<usize>,
}

/// What the rig does, for a run.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Put the arms on the controllers and swing to attack.
    pub rig: bool,
    /// Log the skeleton's elements and the weapon visuals.
    pub probe: bool,
    pub melee: melee::Settings,
    /// With the rig: a free hand's fingers follow the headset's finger tracking.
    pub fingers: bool,
    /// The head model the eyes are drawn with (`head_position`): the hands go where those eyes
    /// see the controllers.
    pub head_position: HeadPosition,
}

impl Options {
    /// The run's: the arms on the controllers (`rig`) or only probed (`probe`); swings attack as
    /// `swing_attack` and `swing_speed` say, the weapon in the aiming hand; the fingers with finger
    /// tracking.
    pub fn read(options: &monaka_core::options::Options, shared: &monaka_core::options::Shared, rig: bool, probe: bool) -> Self {
        Self {
            rig,
            probe,
            melee: melee::Settings::from_options(options, shared.aim.side),
            fingers: rig && shared.fingers.tracking,
            head_position: shared.head_position,
        }
    }
}

pub struct Rig {
    facts: Facts,
    options: Mutex<Option<Options>>,
}

impl Rig {
    pub const fn new(facts: Facts) -> Self {
        Self { facts, options: Mutex::new(None) }
    }

    pub fn facts(&self) -> &Facts {
        &self.facts
    }

    /// The run's options; none means the callback only forwards.
    pub fn options(&self) -> Option<Options> {
        self.options.lock().ok().and_then(|o| *o)
    }

    /// The run's `options`, once the engine has the skeleton access they need.
    pub fn configure(&self, engine: &Module, options: Options) -> Result<(), Rejection> {
        if options.rig || options.probe {
            crate::providers::coskeleton::resolve(engine, &self.facts.skeleton)?;
        }
        *self.options.locked() = Some(options);
        Ok(())
    }

    /// The callback body for `target` (the arms visual's `ICameraTarget` base) and `camera`, around
    /// the game's own call: the rig, when VR runs (`moment.active`), the target is the arms visual's
    /// and the run asked for it.
    pub fn around<G: Chrome>(&self, aim: &HeadAim, target: usize, camera: usize, moment: Moment, original: impl FnOnce()) {
        let Moment { active, head, counter } = moment;
        let Some(options) = self.options().filter(|o| active && (o.rig || o.probe) && aim.is_arms_target(target)) else {
            original();
            return;
        };
        let palms = controllers::palms();
        // A throw under way holds what it throws in the left hand's slot, not the aim hand's.
        let fpp_options = fpp::Options { rig: options.rig, probe: options.probe, melee: options.melee, fingers: options.rig && options.fingers, palms, apart: crate::providers::throws::throwing(), throw_in_hand: false };
        let vis = target.wrapping_sub(self.facts.icamera_target);
        let key = match self.facts.camera_key_at {
            Some(at) => mem::read::<usize>(camera + at).unwrap_or(0),
            None => camera,
        };
        let hands = |game: Mat34| -> Result<Hands, &'static str> {
            if !aim.live() {
                return Err("head aim not steering");
            }
            let head = head.filter(|h| h.valid != 0).ok_or("no head")?;
            let origin = options.head_position.drawn_origin(&aim.tracking_origin(&game, counter).ok_or("camera not levelled")?, &head);
            Ok(Hands::Palms(Palms { game, origin, head, palms, aim_side: options.melee.aim_side, hold: [0.0; 3], bones: None }))
        };
        let outcome = fpp::around::<G, _>(vis, &self.facts.arms, &fpp_options, crate::providers::coskeleton::CoSkeleton::of, aim.player_camera(key), original, hands);
        // The bow by hand's and the melee's posing moment, after the rig: the bow model read,
        // turned toward the left reticle and drawn, where the blade reaches, and the aim hand's
        // path for the swings' replay.
        if options.rig {
            monaka_body::bow::posed::<G>(outcome.placed.is_some(), crate::providers::hands::reticle(LEFT_HAND));
            if outcome.placed.is_some() {
                monaka_body::melee_hit::look::<G>();
                crate::providers::melee::note_posed::<G>(options.melee.aim_side);
            }
        }
    }

    pub fn report(&self) {
        if self.options().is_some() {
            fpp::report();
        }
    }
}

/// What the arms callback sees at a call: whether VR shows the game, the head, and the game frame
/// (for the share head aim baked into it; none where the game numbers no frames).
#[derive(Clone, Copy, Debug)]
pub struct Moment {
    pub active: bool,
    pub head: Option<HeadPose>,
    pub counter: Option<u64>,
}

/// A build's arms: its rig, what each call sees, and whether head aim steers the character at the
/// callback (The Beast: its camera setter only sees the renderer's camera, while the callback has
/// the arms visual, and so the character; the look then hooks the callback).
#[derive(Clone, Copy)]
pub struct Arms {
    pub rig: &'static Rig,
    pub moment: fn() -> Moment,
    pub head_aim_here: bool,
}

/// The game's own callback, under [`camera_target`].
static CAMERA_TARGET: Original<CameraTargetFn> = Original::new();

/// Hooks the arms visual's callback of game `G` ([`camera_target`]) once the vis vtable slot names
/// it; without the rig's options it only forwards (and steers, where head aim steers there).
///
/// # Safety
/// `gamedll` is the build `G`'s facts describe (hash checked by the caller).
pub unsafe fn hook<G: Chrome>(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Rejection> {
    let arms = G::FACTS.arms.ok_or_else(|| Rejection::revision("the arms are not read for this build"))?;
    // SAFETY: the caller's guarantee; the detour has the callback's type.
    unsafe { fpp::hook(hooks, gamedll, &arms.rig.facts.callback, &CAMERA_TARGET, camera_target::<G>) }
}

/// The arms visual's camera-target callback `(ICameraTarget base, camera)`: head aim steers the
/// character first where it steers here (with no head once VR stops, handing the head's share
/// back), then the rig goes around the game's own call ([`Rig::around`]).
unsafe extern "system" fn camera_target<G: Chrome>(target: usize, camera: usize) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let original = || unsafe { CAMERA_TARGET.get()(target, camera) };
    let (Some(arms), Some(look)) = (G::FACTS.arms, G::FACTS.look) else {
        original();
        return;
    };
    let moment = (arms.moment)();
    look.aim.note_target(target);
    if arms.head_aim_here
        && let Some(character) = look.aim.character_of(target)
    {
        look.aim.steer(character, moment.head.filter(|_| moment.active), moment.counter);
    }
    arms.rig.around::<G>(look.aim, target, camera, moment, original);
}
