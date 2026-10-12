//! Providers: the skeleton (the rig on DL2's offsets, [`RIG`], with its posing moment [`moment`]
//! and the hand rig's facts), the held item (`monaka_arms::held::HeldItems`), the throw release
//! (`monaka_arms::launch::Throws`, with `monaka_arms::throw::ThrowsByHand`), the bow's fire hook
//! and draw (`monaka_arms::launch::Fires`, `Draws`, with `monaka_arms::bow::Bows`) and steps
//! (`monaka_arms::feature::Steps`), for the hand rig and what rides on it (`crate::ARMS` and the
//! components after it: fingers, physical melee, the hands' throw, bow and reload, room-scale
//! following).
//!
//! The player camera update calls the arms visual's camera-target callback
//! (`engine::FPP_CAMERA_TARGET`) right after setting the camera; around it the arms are snapshot as
//! animated, then put on the controllers, and melee swings attack (`eng_chr::providers::fpp`).
//! `probe_hands=1` logs the skeleton's elements (name, type, parent, place against the camera) and the weapon
//! visuals' classes.

use crate::Dl2;
use crate::providers::{engine, game};
use monaka_stereo::same_frame::PAIRS;
use eng_chr::providers::rig::{Facts, Moment, Rig};
use monaka_arms::hand_rig::{self, Climbing, HandRig};
use monaka_arms::hand_trail::HandTrails;
use monaka_arms::held::{HeldItems, Item, Line};
use monaka_arms::bow::{Bowing, Bows, Keeps};
use monaka_arms::launch::{Draws, Fires, Throws};
use monaka_arms::melee::Held;
use monaka_arms::melee_hit::Hits;
use monaka_arms::throw::{Throwing, ThrowsByHand};
use monaka_core::camera::Mat34;
use monaka_producer::game::{Lack, Start};
use std::time::Duration;

/// The rig on DL2's offsets.
pub static RIG: Rig = Rig::new(Facts {
    callback: engine::FPP_CAMERA_TARGET,
    icamera_target: engine::FPP_ICAMERA_TARGET,
    skeleton: engine::SKELETON_LAYOUT,
    arms: engine::ARMS_LAYOUT,
    camera_key_at: None,
});

/// What the arms callback sees: VR on, the head the pair is drawn with, the game frame.
pub fn moment() -> Moment {
    Moment { active: PAIRS.running(), head: PAIRS.head().current(), counter: game::get().counter().map(|(_, c)| c as u64) }
}

/// The hand rig (Monaka's) on DL2's bone axes; the climbing measured from the hands, the palms
/// kept for the hands' throw, bow and reload (`eng_chr::providers::hands::PALMS`).
impl HandRig for Dl2 {
    const RIG: hand_rig::Rig = eng_chr::providers::fpp::chrome_rig(engine::BONE_AXES, None, Climbing::Animated);

    fn held(side: usize) -> Held {
        eng_chr::providers::held::kind(side)
    }

    fn holder(side: usize) -> Option<i32> {
        eng_chr::providers::held::holder_of(side)
    }

    fn trail() -> Option<&'static HandTrails> {
        Some(&eng_chr::providers::hands::PALMS)
    }
}

/// The throw release on DL2's throw controller and dropped-item launch (`eng_chr::providers::throws`).
impl Throws for Dl2 {
    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        eng_chr::providers::throws::install_for(start)
    }

    fn throwing() -> bool {
        eng_chr::providers::throws::throwing()
    }
}

/// Throwing by hand on the Chrome games' gains (`eng_chr::providers::throws::THROWING`), from the
/// palms the hand rig notes.
impl ThrowsByHand for Dl2 {
    const THROWING: Throwing = eng_chr::providers::throws::THROWING;
}

/// Melee hits on the Chrome games' hit detection (`eng_chr::providers::melee`); DL2's melee
/// targets are not read, so every swing attacks.
impl Hits for Dl2 {
    const ATTACKS_ON_RELEASE: bool = <Dl2 as eng_chr::feature::Chrome>::FACTS.attacks_on_release;

    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        eng_chr::providers::melee::install_for(start)
    }

    fn install_look(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        eng_chr::providers::melee::install_look_for(start)
    }

    fn near(centre: [f32; 3], radius: f32) -> Option<Vec<(usize, [f32; 6])>> {
        eng_chr::providers::melee::near(centre, radius)
    }

    fn name(id: usize) -> String {
        eng_chr::providers::melee::name(id)
    }
}

/// The fire hook on the bow's arrow (`eng_chr::providers::bow`): each arrow takes its eye point and
/// look from the bow ready-made's answer; its speed is the draw's.
impl Fires for Dl2 {
    const SPEED: bool = false;

    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        eng_chr::providers::bow::install_fire_for(start)
    }
}

/// The draw at the bow's update, set as its power by the game's curve (`eng_chr::providers::bow`).
impl Draws for Dl2 {
    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        eng_chr::providers::bow::install_draw_for(start)
    }

    fn drawn_ago() -> Option<Duration> {
        eng_chr::providers::bow::drawn_ago()
    }
}

/// The bow by hand on the Chrome games' rules (`eng_chr::providers::bow::BOWING`), the bow model's
/// elements placed against their parents by the engine's own local matrices
/// (`eng_chr::providers::bow::KEEPS`).
impl Bows for Dl2 {
    const BOWING: Bowing = eng_chr::providers::bow::BOWING;
    const KEEPS: Keeps = eng_chr::providers::bow::KEEPS;

    fn set_local(model: &Self::Model, element: i32, matrix: &Mat34) -> bool {
        model.set_local(element, matrix)
    }

    fn report() {
        eng_chr::providers::bow::report();
    }
}

/// What the hands hold (`eng_chr::providers::held`): each line read from the held weapon's model
/// (DL2's gun holder is not calibrated: a gun has none).
impl HeldItems for Dl2 {
    type Model = eng_chr::providers::model::Model;

    fn item(side: usize) -> Option<Item> {
        eng_chr::providers::held::item(side)
    }

    fn line(side: usize) -> Option<Line> {
        eng_chr::providers::held::line(side, &eng_chr::providers::held::FROM_MODELS)
    }

    fn model(side: usize) -> Option<Self::Model> {
        eng_chr::providers::held::model(side)
    }
}

/// Room-scale following at the player's movement step (`eng_chr::providers::walk`).
impl monaka_arms::feature::Steps for Dl2 {
    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        eng_chr::feature::hook_steps(start)
    }

    fn report() {
        eng_chr::providers::walk::report();
    }
}
