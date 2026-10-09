//! Room-scale following (`room_scale`) on DL1's walk body: the shared following
//! ([`monaka_arms::roomscale`]) with what only DL1 knows, when following waits and how far a step
//! followed.
//!
//! Each step of the player's walk body ([`crate::player::walk`], 50 Hz), following's velocity goes
//! on top of the game's own wanted velocity. The walk body's step is not measured inside it, so
//! what a step followed is counted at the next one: the body's move since, beyond the game's own
//! wanted velocity.

use crate::player::walk::{self, BODY_POSITION, LOCK_POSITION, PHYSICS_OFF, SCRIPTED_MOVE};
use monaka_arms::roomscale;
use monaka_hook::mem;
use std::sync::Mutex;

/// The body's position at the previous step, and what that step asked: the game's own wanted
/// velocity, the following velocity and the step's length.
struct Last {
    body: Option<[f32; 3]>,
    ask: Option<([f32; 3], [f32; 2], f32)>,
}

static LAST: Mutex<Last> = Mutex::new(Last { body: None, ask: None });

/// Why following waits this step, if it does (climbing is the shared following's own).
fn gate(full: usize) -> Option<&'static str> {
    if !crate::view::stereo::capturing() {
        return Some("not running");
    }
    if mem::read::<u8>(full + SCRIPTED_MOVE).unwrap_or(1) != 0 {
        return Some("scripted move");
    }
    if mem::read::<u8>(full + LOCK_POSITION).unwrap_or(1) != 0 {
        return Some("position locked");
    }
    if mem::read::<u8>(full + PHYSICS_OFF).unwrap_or(1) != 0 {
        return Some("physics off");
    }
    if crate::player::aim::cutscene() {
        return Some("cutscene");
    }
    None
}

/// Before a step of the player's walk body (`full`): credits what the last step followed and
/// returns the velocity to follow with this step (world x and z, m/s).
pub fn before_step(full: usize, dt: f32) -> [f32; 2] {
    if !roomscale::active() {
        return [0.0; 2];
    }
    let Ok(mut last) = LAST.lock() else { return [0.0; 2] };
    let body = walk::body(full);
    let now = if body == 0 { None } else { Some(walk::vec3(body + BODY_POSITION)).filter(|p| p.iter().all(|c| c.is_finite())) };
    if let (Some(now), Some(before), Some((game, follow, last_dt))) = (now, last.body, last.ask) {
        let beyond = [now[0] - before[0] - game[0] * last_dt, 0.0, now[2] - before[2] - game[2] * last_dt];
        roomscale::credit(beyond, follow, last_dt);
    }
    last.body = now;
    let follow = roomscale::follow(gate(full));
    // The game's own wanted velocity is filled in by `asked`, once the walk body has it.
    last.ask = Some(([0.0; 3], follow, dt));
    follow
}

/// The game's own wanted velocity this step (what [`before_step`]'s velocity was added to).
pub fn asked(game: [f32; 3]) {
    if let Ok(mut last) = LAST.lock()
        && let Some(ask) = last.ask.as_mut()
    {
        ask.0 = if game.iter().all(|c| c.is_finite()) { game } else { [0.0; 3] };
    }
}
