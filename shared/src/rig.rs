//! The first-person arms on the controllers (`monaka_arms`) in Dying Light 2 and The Beast (Dying
//! Light 1, whose aim is its own, calls [`crate::fpp::around`] itself): the
//! player camera update calls the arms visual's camera-target callback right after setting the
//! camera; around it the arms are snapshot as animated, then put on the controllers, and melee
//! swings attack ([`crate::fpp::around`]). What both games do around that call is here: the
//! vtable check, the options, the pose the rig goes on (the game's own player camera as
//! head aim noted it, the tracking origin with head aim's share turned out, the head, the
//! controllers). A game supplies its [`Facts`] and, per call, whether VR runs and the head.

use crate::fpp::{self, CameraTargetFn, Layout};
use crate::headaim::HeadAim;
use monaka_arms::{BoneAxes, controllers, melee};
use monaka_core::camera::Mat34;
use monaka_core::protocol::HeadPose;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, Original, mem};
use monaka_producer::Rejection;
use std::sync::Mutex;

/// Where a game keeps its arms (RVAs in the game DLL, offsets in objects).
#[derive(Clone, Copy, Debug)]
pub struct Facts {
    pub callback: fpp::Callback,
    /// The `ICameraTarget` base within the arms visual.
    pub icamera_target: usize,
    pub skeleton: crate::coskeleton::Layout,
    pub arms: Layout,
    pub bones: BoneAxes,
    pub fingers: crate::fingers::Axes,
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

    /// Installs the callback hook (`detour` forwards to [`Rig::around`]) with `options`, once the
    /// engine has the skeleton access and the vis vtable slot names the callback.
    ///
    /// # Safety
    /// `gamedll` is the build the [`Facts`] describe (hash checked by the caller).
    pub unsafe fn install(&self, hooks: &mut Hooks, engine: &Module, gamedll: &Module, options: Options, original: &'static Original<CameraTargetFn>, detour: CameraTargetFn) -> Result<(), Rejection> {
        if options.rig || options.probe {
            crate::coskeleton::resolve(engine, &self.facts.skeleton)?;
        }
        *self.options.lock().unwrap_or_else(|e| e.into_inner()) = Some(options);
        // SAFETY: the caller's guarantee; the detour has the callback's type.
        unsafe { fpp::hook(hooks, gamedll, &self.facts.callback, original, detour) }
    }

    /// The callback body for `target` (the arms visual's `ICameraTarget` base) and `camera`, around
    /// the game's own call: the rig, when VR runs (`active`), the target is the arms visual's and
    /// the run asked for it. `head` is the current head pose and `counter` the game frame (for the
    /// share head aim baked into it).
    pub fn around(&self, aim: &HeadAim, target: usize, camera: usize, active: bool, head: Option<HeadPose>, counter: Option<u64>, original: impl FnOnce()) {
        let Some(options) = self.options().filter(|o| active && (o.rig || o.probe) && aim.is_arms_target(target)) else {
            original();
            return;
        };
        let palms = controllers::palms();
        let fpp_options = fpp::Options { rig: options.rig, probe: options.probe, melee: options.melee, fingers: (options.rig && options.fingers).then_some(self.facts.fingers), palms };
        let vis = target.wrapping_sub(self.facts.icamera_target);
        let key = match self.facts.camera_key_at {
            Some(at) => mem::read::<usize>(camera + at).unwrap_or(0),
            None => camera,
        };
        let pose = |game: Mat34, gun: bool| -> Result<monaka_arms::Pose, &'static str> {
            if !aim.live() {
                return Err("head aim not steering");
            }
            let head = head.filter(|h| h.valid != 0).ok_or("no head")?;
            let origin = aim.tracking_origin(&game, counter).ok_or("camera not levelled")?;
            Ok(monaka_arms::Pose { game, origin, head, palms, aim_side: options.melee.aim_side, hold: [0.0; 3], gun, melee_holder: None, bones: self.facts.bones })
        };
        fpp::around(vis, &self.facts.arms, &fpp_options, crate::coskeleton::CoSkeleton::of, aim.player_camera(key), original, pose);
    }

    pub fn report(&self) {
        if self.options().is_some() {
            fpp::report();
        }
    }
}
