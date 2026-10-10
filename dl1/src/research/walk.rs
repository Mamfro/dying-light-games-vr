//! `probe_walk`: the player's walk body ([`crate::player::walk`]) as the engine steps it. Twice a second it
//! logs how often and with what `dt` the body steps, where the character and the body are and how
//! fast the body goes, the wanted velocity (the stick's, plus room-scale's), the extra velocity
//! and the state fields that make adding velocity unsafe (scripted move-to, locks, the state
//! word, physics off).
//!
//! `walk_push=x,z` (m/s, world axes) also adds that velocity to the wanted velocity: the body
//! moves at that speed and stops at a wall.

use crate::player::walk::{self, BODY_POSITION, BODY_VELOCITY, EXTRA_VELOCITY, LOCK_POSITION, PHYSICS_OFF, SCRIPTED_MOVE, STATE, WANTED_VELOCITY};
use monaka_hook::{Hooks, mem};
use monaka_hook::module::Module;
use monaka_producer::{Rejection, log};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use std::time::Instant;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static PUSH: Mutex<Option<[f32; 2]>> = Mutex::new(None);
static STEPS: AtomicU64 = AtomicU64::new(0);

struct Window {
    started: Option<Instant>,
    last_log: Option<Instant>,
    steps: u32,
    dt: f32,
}

static WINDOW: Mutex<Window> = Mutex::new(Window { started: None, last_log: None, steps: 0, dt: 0.0 });

pub fn install(hooks: &mut Hooks, engine: &Module, push: Option<[f32; 2]>) -> Result<(), Rejection> {
    walk::install(hooks, engine)?;
    *PUSH.lock().unwrap_or_else(|e| e.into_inner()) = push;
    ACTIVE.store(true, Relaxed);
    log!("walk probe: watching the player's walk body; push {push:?} m/s");
    Ok(())
}

/// The test push to add to the wanted velocity this step.
pub fn push() -> Option<[f32; 2]> {
    *PUSH.lock().unwrap_or_else(|e| e.into_inner())
}

/// One step of the player's walk body (`full`, the whole object).
pub fn note(full: usize, dt: f32) {
    if !ACTIVE.load(Relaxed) {
        return;
    }
    STEPS.fetch_add(1, Relaxed);
    let Ok(mut window) = WINDOW.lock() else { return };
    let now = Instant::now();
    let started = *window.started.get_or_insert(now);
    window.steps += 1;
    window.dt += dt;
    if window.last_log.is_some_and(|at| now.duration_since(at).as_secs_f32() < 0.5) {
        return;
    }
    let body = walk::body(full);
    log!(
        "walk probe: {:.2} s {} steps dt {:.4} | player at {:.3?} | body {body:#x} at {:.3?} moving {:.2?} | wanted {:.2?} extra {:.2?} | scripted {} lock {} state {:#x} off {}",
        now.duration_since(started).as_secs_f32(),
        window.steps,
        window.dt / window.steps as f32,
        walk::player_position(),
        walk::vec3(body + BODY_POSITION),
        walk::vec3(body + BODY_VELOCITY),
        walk::vec3(full + WANTED_VELOCITY),
        walk::vec3(full + EXTRA_VELOCITY),
        mem::read::<u8>(full + SCRIPTED_MOVE).unwrap_or(0xff),
        mem::read::<u8>(full + LOCK_POSITION).unwrap_or(0xff),
        mem::read::<u32>(full + STATE).unwrap_or(u32::MAX),
        mem::read::<u8>(full + PHYSICS_OFF).unwrap_or(0xff),
    );
    window.last_log = Some(now);
    window.steps = 0;
    window.dt = 0.0;
}

pub fn report() {
    // No more pushes (the hook is still in) before the wanted velocity goes back to the game.
    *PUSH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if ACTIVE.load(Relaxed) {
        log!("walk probe: {} player steps", STEPS.load(Relaxed));
    }
}
