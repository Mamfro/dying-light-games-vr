//! The bow by hand (`bow_by_hand`, with the hand rig): the bow in the left hand, the string pulled
//! back with the right while the right trigger draws (the game's own draw), let go to loose. The
//! string slides along the bow's own arrow axis, as far back as the right hand's fingers are, and
//! the arrow leaves from the bow's grip along that axis, as hard as the string was pulled.
//!
//! After the rig places the arms ([`place_string`]), while the bow is drawn ([`update`]: its state
//! drawing or drawn), the bow bone is kept in the hand as it rests there (the game's draw moves
//! it), and the bow model's nock and string (`arrow_pose`, `bone_string`) go straight back along
//! the arrow (the nock's axis that lies along the bow's x; forward is away from the string hand)
//! from where the game had them as the draw began, as far as the right hand's fingers are behind
//! that ([`hand_world::string_point`]), at most [`PULL`]'s longest. The bow controller
//! (`WeaponBowController`) looses an arrow ([`loose`]) with a speed from how far the bow is drawn
//! (0 to 1, at +0x6c), set from the pull's share ([`PULL`]'s full is a full draw); the arrow it
//! fires is placed on the arrow's line ([`projectile`], its speed kept). A pull under [`PULL`]'s
//! least keeps the game's draw.

use monaka_core::camera::{apply_3x3, unapply_3x3};
use monaka_core::math::{self, dot, sub};
use crate::arms::{hand_world, hands, projectile};
use monaka_arms::Skeleton;
use monaka_core::gesture::{Pull, PullRules};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::time::{Duration, Instant};

/// `WeaponBowController`'s loosing of an arrow: `(controller)`; its update: `(controller, seconds)`.
const LOOSE: usize = 0xd9cf40;
const UPDATE: usize = 0xd9bfe0;
/// In the controller: its state (1 drawing, 3 drawn).
const STATE: usize = 0x20;
const DRAWING: [i32; 2] = [1, 3];
/// A draw the update has not confirmed for this long is over (the bow put away); a bow placed
/// longer ago than this is not where it was.
const DRAW_FRESH: Duration = Duration::from_millis(200);
/// The bow model's string, and the arrow's nock on it.
const STRING: &str = "bone_string";
const NOCK: &str = "arrow_pose";
/// In the controller: how far it is drawn (0 to 1).
const DRAWN: usize = 0x6c;
/// The pull (metres behind where the string rests) that counts (0.05), that is a full draw (0.5),
/// and the most the string goes back (0.7).
const PULL: PullRules = PullRules::BOW;
/// The bow model's bone the bow bends from (its limbs hang off it).
const BOW_BONE: &str = "bone_root";
/// The arrow's line runs along the bow root's x (one way or the other: forward is away from the
/// string hand).
const ARROW_ALONG: [f32; 3] = [1.0, 0.0, 0.0];
/// The bow is turned to the left controller's line only when within this of it (degrees).
const MOST_TURN: f32 = 30.0;
/// The string hand this far ahead of the nock means forward was taken the wrong way.
const FLIP_BEYOND: f32 = 0.1;

type LooseFn = unsafe extern "C" fn(*mut c_void);
type UpdateFn = unsafe extern "C" fn(*mut c_void, f32);

static LOOSE_ORIGINAL: Original<LooseFn> = Original::new();
static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
/// When the player's bow was last seen drawing or drawn.
static DRAWN_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// The held bow model's elements, how far behind the grip its string rests, which way along its x
/// the arrow points, and its bow bone at rest in the root's frame.
struct Known {
    model: usize,
    string: Option<i32>,
    nock: Option<i32>,
    bow: Option<i32>,
    forward: f32,
    bow_at_rest: Option<[f32; 12]>,
    start: Option<Start>,
}

/// Where a draw began: the nock and the string in the bow root's terms, the nock's axis the arrow
/// lies along, and which way along it is forward.
#[derive(Clone, Copy)]
struct Start {
    nock: Option<[f32; 12]>,
    string: Option<[f32; 12]>,
    arrow: usize,
    sign: f32,
}

static KNOWN: Mutex<Known> = Mutex::new(Known { model: 0, string: None, nock: None, bow: None, forward: 1.0, bow_at_rest: None, start: None });

/// The bow as of the latest arms callback: where the arrow leaves from, its direction (unit), the
/// pull.
#[derive(Clone, Copy)]
struct Bow {
    grip: [f32; 3],
    axis: [f32; 3],
    pull: f32,
    at: Instant,
}

static BOW: Mutex<Option<Bow>> = Mutex::new(None);
/// When the arms were last seen holding a bow.
static HELD_AT: Mutex<Option<Instant>> = Mutex::new(None);
static PLACED: AtomicU64 = AtomicU64::new(0);
static ARROWS: AtomicU64 = AtomicU64::new(0);

pub fn install(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Rejection> {
    // SAFETY: the detour has the function's signature (read from its code); the game DLL's build
    // is checked by the caller; the prologue is decoded and moved.
    unsafe {
        hooks.inline_decoded(&LOOSE_ORIGINAL, "bow loose", gamedll.at(LOOSE), loose as LooseFn)?;
        hooks.inline_decoded(&UPDATE_ORIGINAL, "bow update", gamedll.at(UPDATE), update as UpdateFn)?;
    }
    projectile::install(hooks, gamedll)?;
    log!("bow by hand: the string drawn along the bow by the right hand, the arrow loosed along the bow");
    Ok(())
}

unsafe extern "C" fn loose(controller: *mut c_void) {
    let _flight = InFlight::enter();
    let at = controller as usize;
    let bow = BOW.lock().ok().and_then(|b| *b).filter(|b| b.at.elapsed() < DRAW_FRESH);
    let shot = bow.filter(|_| hands::players(at)).map(|bow| {
        if let Some(share) = Pull::share(bow.pull, &PULL) {
            mem::write::<f32>(at + DRAWN, share);
        }
        projectile::Shot::Along { start: bow.grip, direction: bow.axis }
    });
    if let Some(shot) = shot {
        let n = ARROWS.fetch_add(1, Relaxed) + 1;
        if n <= 20 {
            log!("bow by hand: arrow {n}: {shot:.2?}, pulled {:.2} m", bow.map_or(0.0, |b| b.pull));
        }
    }
    // SAFETY: forwards the game's own call.
    projectile::aim(shot, || unsafe { LOOSE_ORIGINAL.get()(controller) });
}

unsafe extern "C" fn update(controller: *mut c_void, seconds: f32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(controller, seconds) };
    let at = controller as usize;
    if hands::players(at) {
        let drawing = mem::read::<i32>(at + STATE).is_some_and(|s| DRAWING.contains(&s));
        if let Ok(mut drawn) = DRAWN_AT.lock() {
            *drawn = drawing.then(Instant::now);
        }
    }
}

/// Whether the arms hold a bow (seen at the latest arms callbacks), with the bow by hand on.
pub fn held() -> bool {
    HELD_AT.lock().ok().and_then(|h| *h).is_some_and(|at| at.elapsed() < DRAW_FRESH)
}

fn drawing() -> bool {
    DRAWN_AT.lock().ok().and_then(|d| *d).is_some_and(|at| at.elapsed() < DRAW_FRESH)
}

/// After the rig placed the arms: where the bow and its axis are; while the bow is drawn, its
/// string and the arrow's nock on the axis, level with the right hand's fingers.
pub fn place_string(vis: usize, reticle: Option<[f32; 3]>) {
    if !LOOSE_ORIGINAL.is_set() {
        return;
    }
    let Some(model) = hands::held_bow(vis) else { return };
    if let Ok(mut at) = HELD_AT.lock() {
        *at = Some(Instant::now());
    }
    let Some(mut root) = model.world(0) else { return };
    let Ok(mut known) = KNOWN.lock() else { return };
    if known.model != model.id() {
        use crate::arms::hands::find_element;
        *known = Known {
            model: model.id(),
            string: find_element(&model, STRING),
            nock: find_element(&model, NOCK),
            bow: find_element(&model, BOW_BONE),
            forward: 1.0,
            bow_at_rest: None,
            start: None,
        };
        log!("bow by hand: the string drawn along the bow (string {:?}, nock {:?}, bow {:?})", known.string, known.nock, known.bow);
    }
    let along = apply_3x3(&root, ARROW_ALONG);
    let length = math::length(along);
    if !(length.is_finite() && length > 1e-6) {
        return;
    }
    let mut bow_x = along.map(|c| c / length);
    // The bow turned in the hand (about its grip) so the arrow points at the viewer's reticle: a
    // few degrees, the same each update.
    if let Some(turned) = reticle.and_then(|r| arrow_line(&model, &known, &root, bow_x).and_then(|(at, arrow)| aligned(&root, arrow, sub(r, at)))) {
        model.set_world(0, &turned);
        root = turned;
        let along = apply_3x3(&root, ARROW_ALONG);
        bow_x = along.map(|c| c / math::length(along));
    }
    let grip = [root[3], root[7], root[11]];
    let drawn = drawing();
    let fingers = hand_world::string_point().filter(|p| p.iter().all(|c| c.is_finite()));
    if !drawn {
        known.start = None;
        known.bow_at_rest = known.bow.and_then(|e| model.world(e)).and_then(|m| in_frame(&root, &m));
        if let Ok(mut bow) = BOW.lock() {
            // The pull of the draw is kept past its end: the bow leaves the drawing state as it
            // is let go, a moment before it looses.
            let kept = bow.filter(|b| b.at.elapsed() < DRAW_FRESH);
            *bow = Some(kept.unwrap_or(Bow { grip, axis: bow_x.map(|c| c * known.forward), pull: 0.0, at: Instant::now() }));
        }
        return;
    }
    // The draw's start: the nock and the string where the game has them, in the bow's terms, and
    // which of the nock's axes is the arrow's (the one along the bow's x).
    if known.start.is_none() {
        let nock = known.nock.and_then(|e| model.world(e));
        let arrow = nock.map_or(0, |m| (0..3).max_by(|&a, &b| column_along(&m, a, bow_x).abs().total_cmp(&column_along(&m, b, bow_x).abs())).unwrap_or(0));
        let sign = nock.map_or(1.0, |m| column_along(&m, arrow, bow_x).signum()) * known.forward;
        known.start = Some(Start {
            nock: nock.and_then(|m| in_frame(&root, &m)),
            string: known.string.and_then(|e| model.world(e)).and_then(|m| in_frame(&root, &m)),
            arrow,
            sign,
        });
    }
    let Some(start) = known.start else { return };
    let Some(nock) = start.nock.map(|local| out_of_frame(&root, &local)) else { return };
    let from = [nock[3], nock[7], nock[11]];
    let column = [nock[start.arrow], nock[4 + start.arrow], nock[8 + start.arrow]];
    let column_length = math::length(column);
    if !(column_length.is_finite() && column_length > 1e-6) {
        return;
    }
    let mut axis = column.map(|c| c * start.sign / column_length);
    // Forward points away from the string hand, which is behind the bow while it draws.
    if let Some(f) = fingers
        && dot(sub(from, f), axis) < -FLIP_BEYOND
    {
        known.start = Some(Start { sign: -start.sign, ..start });
        known.forward = -known.forward;
        axis = axis.map(|c| -c);
        log!("bow by hand: the arrow points the other way along the nock's axis");
    }
    let pull = fingers.map_or(0.0, |f| Pull::back(from, axis, f, &PULL));
    if let Ok(mut bow) = BOW.lock() {
        *bow = Some(Bow { grip: from, axis, pull, at: Instant::now() });
    }
    // The bow stays in the hand as it rests there (the game's draw moves it), then the nock and the
    // string go straight back along the arrow from where the draw began.
    if let (Some(e), Some(local)) = (known.bow, known.bow_at_rest) {
        model.set_world(e, &out_of_frame(&root, &local));
    }
    let back = |m: [f32; 12]| {
        let mut m = m;
        for k in 0..3 {
            m[k * 4 + 3] -= axis[k] * pull;
        }
        m
    };
    if let Some(e) = known.nock {
        model.set_world(e, &back(nock));
    }
    if let (Some(e), Some(local)) = (known.string, start.string) {
        model.set_world(e, &back(out_of_frame(&root, &local)));
    }
    PLACED.fetch_add(1, Relaxed);
}

/// Where the arrow is and which way it points (unit): the nock as the draw began (moved with the
/// bow), else as the game has it, along its axis that lies along the bow's x.
fn arrow_line(model: &hands::Model, known: &Known, root: &[f32; 12], bow_x: [f32; 3]) -> Option<([f32; 3], [f32; 3])> {
    let (nock, arrow, sign) = match known.start {
        Some(start) => (out_of_frame(root, &start.nock?), start.arrow, start.sign),
        None => {
            let nock = model.world(known.nock?)?;
            let arrow = (0..3).max_by(|&a, &b| column_along(&nock, a, bow_x).abs().total_cmp(&column_along(&nock, b, bow_x).abs()))?;
            (nock, arrow, column_along(&nock, arrow, bow_x).signum() * known.forward)
        }
    };
    let column = [nock[arrow], nock[4 + arrow], nock[8 + arrow]];
    let length = math::length(column);
    (length > 1e-6).then(|| ([nock[3], nock[7], nock[11]], column.map(|c| c * sign / length)))
}

/// `root` turned about its own position so `arrow` (unit) points along `toward`; `None` when they
/// are further apart than [`MOST_TURN`] (the bow then is not being aimed).
fn aligned(root: &[f32; 12], arrow: [f32; 3], toward: [f32; 3]) -> Option<[f32; 12]> {
    let length = math::length(toward);
    if !length.is_finite() || length <= 1e-6 {
        return None;
    }
    let (from, to) = (arrow, toward.map(|c| c / length));
    if dot(from, to) < MOST_TURN.to_radians().cos() {
        return None;
    }
    let column = |c: usize| projectile::turned([root[c], root[4 + c], root[8 + c]], from, to);
    let [a, b, c] = [column(0), column(1), column(2)];
    Some([a[0], b[0], c[0], root[3], a[1], b[1], c[1], root[7], a[2], b[2], c[2], root[11]])
}

/// How far `m`'s column `c` (made unit) runs along unit `along`.
fn column_along(m: &[f32; 12], c: usize, along: [f32; 3]) -> f32 {
    let column = [m[c], m[4 + c], m[8 + c]];
    let length = math::length(column);
    if length > 1e-6 { dot(column, along) / length } else { 0.0 }
}

/// `m` in `frame`'s own terms (its 3x3 part may scale): `frame` applied to it gives `m` back.
fn in_frame(frame: &[f32; 12], m: &[f32; 12]) -> Option<[f32; 12]> {
    let columns = [0, 1, 2].map(|c| unapply_3x3(frame, [m[c], m[4 + c], m[8 + c]]));
    let at = unapply_3x3(frame, sub([m[3], m[7], m[11]], [frame[3], frame[7], frame[11]]))?;
    let [x, y, z] = [columns[0]?, columns[1]?, columns[2]?];
    Some([x[0], y[0], z[0], at[0], x[1], y[1], z[1], at[1], x[2], y[2], z[2], at[2]])
}

/// `local` (from [`in_frame`]) put back through `frame`.
fn out_of_frame(frame: &[f32; 12], local: &[f32; 12]) -> [f32; 12] {
    let column = |c: usize| apply_3x3(frame, [local[c], local[4 + c], local[8 + c]]);
    let [x, y, z] = [column(0), column(1), column(2)];
    let at = apply_3x3(frame, [local[3], local[7], local[11]]);
    [x[0], y[0], z[0], frame[3] + at[0], x[1], y[1], z[1], frame[7] + at[1], x[2], y[2], z[2], frame[11] + at[2]]
}

pub fn report() {
    let n = ARROWS.load(Relaxed);
    if n > 0 {
        log!("bow by hand: {n} arrows loosed by hand");
    }
    let placed = PLACED.load(Relaxed);
    if placed > 0 {
        log!("bow by hand: the string drawn along the bow {placed} times");
    }
}
