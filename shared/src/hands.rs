//! The controllers' palms in the game world, as the hand rig puts the arms on them ([`crate::rig`]):
//! noted each time the rig builds its pose, kept a short while, for what the hands do outside the
//! arms' own update (a throw leaving the hand, a bow's draw, a gesture). Each note is the tracking
//! origin the arms stand on (the levelled player camera, head aim's share turned out, room-scale
//! following's walk) and each tracked palm on it: the controllers themselves, not the arms the game
//! animates (an animated arm lags its controller, and rides the gun when the game holds it with both
//! hands).

use monaka_core::camera::{Mat34, apply_head};
use monaka_core::protocol::HandPose;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Instant;

/// How long each hand's way is kept (seconds).
const KEPT: f32 = 0.8;
/// A velocity is read over at least this long (seconds): a few game frames.
pub const VELOCITY_SPAN: f32 = 0.06;
/// Notes closer together than this (seconds) are one update's (several callbacks give one pose).
const SAME_UPDATE: f32 = 0.002;

type Trail = VecDeque<(Instant, Mat34)>;

/// Each hand's recent palms in the world: (when, palm-to-world matrix), oldest first.
static TRAILS: Mutex<[Trail; 2]> = Mutex::new([VecDeque::new(), VecDeque::new()]);

/// Notes the palms placed on `origin` now (from the rig's pose).
pub fn note(origin: &Mat34, palms: &[Option<HandPose>; 2]) {
    let now = Instant::now();
    let Ok(mut trails) = TRAILS.lock() else { return };
    for (side, trail) in trails.iter_mut().enumerate() {
        if let Some(palm) = palms[side].filter(|p| p.valid != 0) {
            let world = apply_head(origin, palm.orientation, palm.position);
            let new = trail.back().is_none_or(|(at, _)| now.duration_since(*at).as_secs_f32() > SAME_UPDATE);
            if new && world.iter().all(|v| v.is_finite()) {
                trail.push_back((now, world));
            }
        }
        while trail.front().is_some_and(|(t, _)| now.duration_since(*t).as_secs_f32() > KEPT) {
            trail.pop_front();
        }
    }
}

/// Hand `side`'s latest palm-to-world matrix, if it was noted within `fresh` seconds.
pub fn palm(side: usize, fresh: f32) -> Option<Mat34> {
    let trails = TRAILS.lock().ok()?;
    let &(at, world) = trails.get(side)?.back()?;
    (at.elapsed().as_secs_f32() <= fresh).then_some(world)
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

/// The fastest velocity in `trail` in the last `within` seconds, each read over [`VELOCITY_SPAN`],
/// and the note it ends at.
fn peak(trail: &Trail, within: f32) -> Option<([f32; 3], usize)> {
    let &(newest, _) = trail.back()?;
    let mut best: Option<([f32; 3], usize)> = None;
    for (end, &(t_end, m_end)) in trail.iter().enumerate().rev() {
        if newest.duration_since(t_end).as_secs_f32() > within {
            break;
        }
        // The latest note at least a span earlier (none for the older ends either).
        let Some(&(t_start, m_start)) = trail.iter().take(end).rev().find(|(t, _)| t_end.duration_since(*t).as_secs_f32() >= VELOCITY_SPAN) else { break };
        let dt = t_end.duration_since(t_start).as_secs_f32().max(1e-3);
        let (a, b) = (position(&m_start), position(&m_end));
        let velocity = [0, 1, 2].map(|k| (b[k] - a[k]) / dt);
        if best.is_none_or(|(v, _)| length(velocity) > length(v)) {
            best = Some((velocity, end));
        }
    }
    best
}

/// Hand `side`'s fastest velocity (world, m/s) over the last `within` seconds. An action's speed is
/// its peak: by the time a button is let go the hand may already be slowing.
pub fn fastest(side: usize, within: f32) -> Option<[f32; 3]> {
    let trails = TRAILS.lock().ok()?;
    peak(trails.get(side)?, within).map(|(v, _)| v)
}

/// Which way hand `side` travelled through its fastest moment in the last `within` seconds (unit):
/// from `before` seconds before that moment to `after` seconds after it (the newest note at most).
/// `None` when it hardly moved. Steadier than one span's velocity, and a flick that stopped before
/// the button was let go keeps its way.
pub fn stroke(side: usize, within: f32, before: f32, after: f32) -> Option<[f32; 3]> {
    let trails = TRAILS.lock().ok()?;
    let trail = trails.get(side)?;
    let (_, at) = peak(trail, within)?;
    let (peak_at, _) = trail[at];
    let (_, from) = *trail.iter().take(at + 1).find(|(t, _)| peak_at.duration_since(*t).as_secs_f32() <= before)?;
    let (_, to) = *trail.iter().skip(at).take_while(|(t, _)| t.duration_since(peak_at).as_secs_f32() <= after).last()?;
    let moved = [0, 1, 2].map(|k| position(&to)[k] - position(&from)[k]);
    let length = length(moved);
    (length > 0.005).then(|| moved.map(|c| c / length))
}

pub fn length(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

pub fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(x: f32, y: f32) -> Mat34 {
        [1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, y, 0.0, 0.0, 1.0, 0.0]
    }

    #[test]
    fn the_peak_is_the_fastest_stretch_and_the_stroke_its_way() {
        let start = Instant::now();
        let mut trails = TRAILS.lock().unwrap();
        trails[0].clear();
        // 0.02 s steps: 0.1 m each along x (5 m/s), then slowing to 0.02 m each (1 m/s) while
        // drifting down along y.
        let (mut x, mut y) = (0.0, 0.0);
        for i in 0..20 {
            if i < 10 {
                x += 0.1;
            } else {
                x += 0.02;
                y -= 0.05;
            }
            trails[0].push_back((start + Duration::from_millis(20 * i), at(x, y)));
        }
        drop(trails);
        let velocity = fastest(0, 1.0).unwrap();
        assert!((velocity[0] - 5.0).abs() < 0.01 && velocity[1].abs() < 0.01, "{velocity:?}");
        let late = fastest(0, 0.1).unwrap();
        assert!((late[0] - 1.0).abs() < 0.01, "only the slow end is recent: {late:?}");
        // Through the peak the hand went along x: the slow drift after it is not the throw.
        let way = stroke(0, 1.0, 0.1, 0.04).unwrap();
        assert!(way[0] > 0.95, "{way:?}");
    }
}
