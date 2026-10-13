//! Providers: the skeleton (the rig on The Beast's offsets, [`RIG`], with its posing moment
//! [`moment`] and the hand rig's facts), the held item (`monaka_body::held::HeldItems`), the throw
//! release (`monaka_body::launch::Throws`, with `monaka_body::throw::ThrowsByHand`), the bow's
//! fire hook and draw (`monaka_body::launch::Fires`, `Draws`, with `monaka_body::bow::Bows`) and
//! steps (`monaka_body::feature::Steps`), for the hand rig and what rides on it (`crate::HAND_RIG` and
//! the components after it: fingers, physical melee, the hands' throw, bow and reload, room-scale
//! following).
//!
//! As in Dying Light 2, the player camera update calls the arms visual's camera-target callback
//! (`engine::FPP_CAMERA_TARGET`) right after setting the camera. The camera the callback gets is
//! already the eye's ([`eye_site`]); the arms go against the game's own camera, noted before the
//! eye replaced it (`eng_chr::providers::look_fields::HeadAim::player_update`, keyed by the renderer's
//! camera behind the game's). Head aim steers the character there too, so the look hooks the
//! callback (`head_aim_here`) and the arms only configure it. `probe_hands=1` (with stereo) logs
//! the skeleton's elements and the weapon visuals' classes.

use crate::Beast;
use crate::providers::{controllers, engine, eye_site};
use eng_chr::providers::rig::{Facts, Moment, Rig};
use monaka_body::hand_rig::{self, Climbing, HandRig};
use monaka_body::hand_trail::HandTrails;
use monaka_body::held::{HeldItems, Item, Kind, Line};
use monaka_body::bow::{Bowing, Bows, Keeps};
use monaka_body::launch::{Draws, Fires, Throws};
use monaka_body::melee_hit::Hits;
use monaka_body::throw::{Throwing, ThrowsByHand};
use monaka_core::camera::Mat34;
use monaka_producer::game::{Lack, Start};
use std::time::Duration;

/// The rig on The Beast's offsets.
pub static RIG: Rig = Rig::new(Facts {
    callback: engine::FPP_CAMERA_TARGET,
    icamera_target: engine::FPP_ICAMERA_TARGET,
    skeleton: engine::SKELETON_LAYOUT,
    arms: engine::ARMS_LAYOUT,
    camera_key_at: Some(engine::CAMERA_RENDERER),
});

/// What the arms callback sees: stereo running and the present loop's head (no game frame). The
/// game's word on climbing goes to the hand rig here, before the arms are posed
/// ([`controllers`]).
pub fn moment() -> Moment {
    if let Some(climbing) = controllers::climbing() {
        monaka_body::traverse::report_climbing(climbing);
    }
    Moment { active: eye_site::running(), head: monaka_stereo::feature::DRIVER.head().current(), counter: None }
}

/// The hand rig (Monaka's) on The Beast's bone axes and left mirroring; the climbing said by the
/// game ([`controllers`]), the palms kept for the hands' throw, bow and reload
/// (`eng_chr::providers::hands::PALMS`).
impl HandRig for Beast {
    const RIG: hand_rig::Rig = eng_chr::providers::fpp::chrome_rig(engine::BONE_AXES, Some(engine::LEFT_MIRROR), Climbing::Reported);

    fn held(side: usize) -> Kind {
        eng_chr::providers::held::kind(side)
    }

    fn item(side: usize) -> Option<Item> {
        eng_chr::providers::held::item(side)
    }

    fn holder(side: usize) -> Option<i32> {
        eng_chr::providers::held::holder_of(side)
    }

    fn trail() -> Option<&'static HandTrails> {
        Some(&eng_chr::providers::hands::PALMS)
    }
}

/// The throw release on The Beast's throw controller and dropped-item launch
/// (`eng_chr::providers::throws`).
impl Throws for Beast {
    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        eng_chr::providers::throws::install_for(start)
    }

    fn throwing() -> bool {
        eng_chr::providers::throws::throwing()
    }
}

/// Throwing by hand on the Chrome games' gains (`eng_chr::providers::throws::THROWING`), from the
/// palms the hand rig notes.
impl ThrowsByHand for Beast {
    const THROWING: Throwing = eng_chr::providers::throws::THROWING;
}

/// Melee hits on the Chrome games' hit detection and its melee targets
/// (`eng_chr::providers::melee`).
impl Hits for Beast {
    const ATTACKS_ON_RELEASE: bool = <Beast as eng_chr::feature::Chrome>::FACTS.attacks_on_release;

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
impl Fires for Beast {
    const SPEED: bool = false;

    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        eng_chr::providers::bow::install_fire_for(start)
    }
}

/// The draw at the bow's update, set as its power by the game's curve (`eng_chr::providers::bow`).
impl Draws for Beast {
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
impl Bows for Beast {
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
/// (The Beast's gun holder is not calibrated: a gun has none).
impl HeldItems for Beast {
    type Model = eng_chr::providers::model::Model;

    fn line(side: usize) -> Option<Line> {
        eng_chr::providers::held::line(side, &eng_chr::providers::held::FROM_MODELS)
    }

    fn model(side: usize) -> Option<Self::Model> {
        eng_chr::providers::held::model(side)
    }
}

/// Room-scale following at the player's movement step (`eng_chr::providers::walk`).
impl monaka_body::feature::Steps for Beast {
    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        eng_chr::feature::hook_steps(start)
    }

    fn report() {
        eng_chr::providers::walk::report();
    }
}
