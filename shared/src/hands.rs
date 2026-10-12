//! The controllers' palms in the game world, as the hand rig puts the arms on them ([`crate::rig`]):
//! noted each time the rig builds its pose, kept a short while, for what the hands do outside the
//! arms' own update (a throw leaving the hand, a bow's draw, a gesture). Each note is the tracking
//! origin the arms stand on (the levelled player camera, head aim's share turned out, room-scale
//! following's walk) and each tracked palm on it: the controllers themselves, not the arms the game
//! animates (an animated arm lags its controller, and rides the gun when the game holds it with both
//! hands). Each palm's way is a trail (`monaka_core::trail`), for its speed and stroke.

use monaka_core::camera::{Mat34, apply_head};
use monaka_core::protocol::HandPose;
use monaka_core::trail::Trail;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// How long each hand's way is kept (seconds).
const KEPT: f64 = 0.8;
/// A velocity is read over at least this long (seconds): a few game frames.
pub const VELOCITY_SPAN: f64 = 0.06;

/// One hand: its palm's way in the world, and the latest palm-to-world matrix noted on it.
struct Hand {
    trail: Trail,
    latest: Option<Mat34>,
}

static HANDS: Mutex<[Hand; 2]> = Mutex::new([Hand { trail: Trail::new(KEPT), latest: None }, Hand { trail: Trail::new(KEPT), latest: None }]);
/// The trails' clock.
static STARTED: OnceLock<Instant> = OnceLock::new();

/// Seconds on the trails' clock, which the hand gestures keep time by too.
pub fn now() -> f64 {
    STARTED.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Notes the palms placed on `origin` now (from the rig's pose). Several notes in one update give
/// one pose: the first is kept ([`Trail::SAME_UPDATE`]).
pub fn note(origin: &Mat34, palms: &[Option<HandPose>; 2]) {
    let now = now();
    let Ok(mut hands) = HANDS.lock() else { return };
    for (side, hand) in hands.iter_mut().enumerate() {
        if let Some(palm) = palms[side].filter(|p| p.valid != 0) {
            let world = apply_head(origin, palm.orientation, palm.position);
            if world.iter().all(|v| v.is_finite()) && hand.trail.push_position(now, position(&world)) {
                hand.latest = Some(world);
            }
        }
        hand.trail.expire(now);
    }
}

/// Hand `side`'s latest palm-to-world matrix, if it was noted within `fresh` seconds.
pub fn palm(side: usize, fresh: f32) -> Option<Mat34> {
    let now = now();
    let hands = HANDS.lock().ok()?;
    let hand = hands.get(side)?;
    hand.trail.fresh(now, f64::from(fresh))?;
    hand.latest
}

/// The position in a palm-to-world matrix.
pub fn position(world: &Mat34) -> [f32; 3] {
    [world[3], world[7], world[11]]
}

/// Where a palm points (its local -z) in the world: the line the viewer's reticle for that hand
/// sits on.
pub fn forward(world: &Mat34) -> [f32; 3] {
    [-world[2], -world[6], -world[10]]
}

/// Hand `side`'s fastest velocity (world, m/s, each over [`VELOCITY_SPAN`]) over the last `within`
/// seconds. An action's speed is its peak: by the time a button is let go the hand may already be
/// slowing.
pub fn fastest(side: usize, within: f32) -> Option<[f32; 3]> {
    let hands = HANDS.lock().ok()?;
    hands.get(side)?.trail.fastest(f64::from(within), VELOCITY_SPAN).map(|peak| peak.velocity)
}

/// Which way hand `side` travelled through its fastest moment in the last `within` seconds (unit):
/// from `before` seconds before that moment to `after` seconds after it (the newest note at most).
/// `None` when it hardly moved. Steadier than one span's velocity, and a flick that stopped before
/// the button was let go keeps its way.
pub fn stroke(side: usize, within: f32, before: f32, after: f32) -> Option<[f32; 3]> {
    let hands = HANDS.lock().ok()?;
    hands.get(side)?.trail.stroke(f64::from(within), VELOCITY_SPAN, f64::from(before), f64::from(after))
}
