//! Provider: the hands in the game world (the tracked palms on the tracking origin, with their
//! trails), which the hand rig hands the features riding on it (`HandRig::trail`); and the
//! addresses of what the hands do with the game's own throw, bow and lockpick ([`Builds`]).
//!
//! The controllers' palms in the game world, as the hand rig puts the arms on them
//! ([`crate::providers::rig`]): noted each time the rig builds its pose, kept a short while, for
//! what the hands do outside the arms' own update (a throw leaving the hand, a bow's draw, a
//! gesture). Each note is the tracking origin the arms stand on (the levelled player camera, head
//! aim's share turned out, room-scale following's walk) and each tracked palm on it: the
//! controllers themselves, not the arms the game animates (an animated arm lags its controller, and
//! rides the gun when the game holds it with both hands). Each palm's way is a trail
//! (`monaka_body::hand_trail`), for its speed and stroke, kept while a controller is briefly lost
//! (it empties as time goes on).

use monaka_body::hand_trail::{HandTrails, Lost, TrailRules};
use monaka_core::camera::Mat34;

/// One build's addresses for what the hands do with the game's own throw, bow and lockpick (none
/// where the build has not been read for it): the providers' hooks behind the throw, bow and
/// lockpick ready-mades.
#[derive(Clone, Copy, Debug)]
pub struct Builds {
    pub throw: Option<crate::providers::throws::Build>,
    pub weapon_throw: Option<crate::providers::throws::WeaponBuild>,
    pub aim: crate::providers::aim::Build,
    pub bow: Option<crate::providers::bow::Build>,
    pub lockpick: Option<crate::providers::lockpick::Build>,
}

/// Each palm's way: kept 0.8 s, a velocity read over at least 0.06 s (a few game frames).
pub const TRAILS: TrailRules = TrailRules { kept: 0.8, span: 0.06, lost: Lost::Kept };

/// The palms in the world, noted each time the rig builds its pose.
pub static PALMS: HandTrails = HandTrails::new(TRAILS);

/// The position in a palm-to-world matrix.
pub fn position(world: &Mat34) -> [f32; 3] {
    [world[3], world[7], world[11]]
}

/// Where a palm points (its local -z) in the world: the line the viewer's reticle for that hand
/// sits on.
pub fn forward(world: &Mat34) -> [f32; 3] {
    [-world[2], -world[6], -world[10]]
}

/// Where the viewer's reticle for hand `side` sits in the world: `WEAPON_CONVERGENCE` metres along
/// where its palm, noted within 0.1 s, points.
pub fn reticle(side: usize) -> Option<[f32; 3]> {
    let palm = PALMS.palm(side, 0.1)?;
    let (at, way) = (position(&palm), forward(&palm));
    Some([0, 1, 2].map(|k| at[k] + way[k] * monaka_core::math::WEAPON_CONVERGENCE))
}
