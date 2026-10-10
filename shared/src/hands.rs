//! The controllers' palms in the game world, as the hand rig puts the arms on them ([`crate::rig`]):
//! noted each time the rig builds its pose, kept a short while, for what the hands do outside the
//! arms' own update (a throw leaving the hand, a bow's draw, a gesture). Each note is the tracking
//! origin the arms stand on (the levelled player camera, head aim's share turned out, room-scale
//! following's walk) and each tracked palm on it.

use monaka_core::camera::{Mat34, apply_head};
use monaka_core::protocol::HandPose;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Instant;

/// How long each hand's way is kept (seconds).
const KEPT: f32 = 0.6;
/// A velocity is read over at least this long (seconds): a few game frames.
pub const VELOCITY_SPAN: f32 = 0.06;

/// Each hand's recent palms in the world: (when, palm-to-world matrix), oldest first.
static TRAILS: Mutex<[VecDeque<(Instant, Mat34)>; 2]> = Mutex::new([VecDeque::new(), VecDeque::new()]);

/// Notes the palms placed on `origin` now (from the rig's pose).
pub fn note(origin: &Mat34, palms: &[Option<HandPose>; 2]) {
    let now = Instant::now();
    let Ok(mut trails) = TRAILS.lock() else { return };
    for (side, trail) in trails.iter_mut().enumerate() {
        if let Some(palm) = palms[side].filter(|p| p.valid != 0) {
            let world = apply_head(origin, palm.orientation, palm.position);
            if world.iter().all(|v| v.is_finite()) {
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

/// Where a palm points (its local -z) in the world.
pub fn forward(world: &Mat34) -> [f32; 3] {
    [-world[2], -world[6], -world[10]]
}

/// Hand `side`'s fastest velocity (world, m/s) over the last `within` seconds, each read over
/// [`VELOCITY_SPAN`], and where the hand was at its end; with `toward`, only motion with some part
/// along it (a throw's wind-up, moving back, can be as fast as the throw). A throw's speed is its
/// peak: by the time a button is let go the hand may already be slowing.
pub fn peak_velocity(side: usize, within: f32, toward: Option<[f32; 3]>) -> Option<([f32; 3], [f32; 3])> {
    let trails = TRAILS.lock().ok()?;
    let trail = trails.get(side)?;
    let &(newest, _) = trail.back()?;
    let mut best: Option<(f32, [f32; 3], [f32; 3])> = None;
    for (end, &(t_end, m_end)) in trail.iter().enumerate().rev() {
        if newest.duration_since(t_end).as_secs_f32() > within {
            break;
        }
        let Some(&(t_start, m_start)) = trail.iter().take(end).rev().find(|(t, _)| t_end.duration_since(*t).as_secs_f32() >= VELOCITY_SPAN) else { break };
        let dt = t_end.duration_since(t_start).as_secs_f32().max(1e-3);
        let (a, b) = (position(&m_start), position(&m_end));
        let velocity = [0, 1, 2].map(|k| (b[k] - a[k]) / dt);
        let speed = length(velocity);
        if toward.is_some_and(|d| d[0] * velocity[0] + d[1] * velocity[1] + d[2] * velocity[2] <= 0.0) {
            continue;
        }
        if best.is_none_or(|(s, _, _)| speed > s) {
            best = Some((speed, velocity, b));
        }
    }
    best.map(|(_, velocity, at)| (velocity, at))
}

pub fn length(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(x: f32) -> Mat34 {
        [1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]
    }

    #[test]
    fn the_peak_is_the_fastest_stretch_not_the_last() {
        let start = Instant::now();
        let mut trails = TRAILS.lock().unwrap();
        trails[0].clear();
        // 0.02 s steps: 0.1 m each (5 m/s), then slowing to 0.02 m each (1 m/s).
        let mut x = 0.0;
        for i in 0..20 {
            x += if i < 10 { 0.1 } else { 0.02 };
            trails[0].push_back((start + Duration::from_millis(20 * i), at(x)));
        }
        drop(trails);
        let (velocity, _) = peak_velocity(0, 1.0, None).unwrap();
        assert!((velocity[0] - 5.0).abs() < 0.01, "{velocity:?}");
        let (late, _) = peak_velocity(0, 0.1, None).unwrap();
        assert!((late[0] - 1.0).abs() < 0.01, "only the slow end is recent: {late:?}");
        assert!(peak_velocity(0, 1.0, Some([-1.0, 0.0, 0.0])).is_none(), "no motion the wanted way");
    }
}
