//! The bow by hand (with the hand rig): the bow in the left hand, the string in the right (the rig
//! holds them apart, [`crate::fpp::is_bow`]). Holding fire draws as before; how far the string hand
//! is pulled back along the bow's line sets the draw's power, and the arrow leaves straight out of
//! the bow as the hand holds it, instead of from the eye along the look. While a bow is held the
//! aim stays the head's ([`super::head_aims`]).
//!
//! The bow's line is the bow model's own, read from its shape alone ([`note_bow`]): the arrow's
//! nock sits on the string, behind the bow bone it hangs from, so the arrow runs from the nock to
//! that bone, along the nock's axis lying that way. The line is kept against the model's root
//! (`holder`), read from the one model at once, so the two agree however lately the engine moved the
//! bow. The root sits on the left hand's holder, so wherever the rig puts the holder
//! ([`note_placed`]), the arrow points along the line from it, and the left hand's reticle with it
//! (told to the viewer in the palm's frame). A model without such a nock has no line: the left
//! controller's pointing aims instead, as it does before a bow is first read and for a line pointing
//! back from the controller.
//!
//! The pull is how far the right hand is behind where the arrow's nock rests along that line. The
//! game's draw would take the bow model's string and nock all the way back by itself; while it
//! draws they go back as far as the hand is instead ([`place_string`]).
//!
//! The bow controller (`WeaponBowController`) draws in one state: each update it works the power
//! out from the time drawn over the full draw time `t`, as the most power times `1 - (1 - t)^2`.
//! Here, after each update in that state, the power is set by the same curve from the pull's share
//! instead (and kept as the bow looses); the game's own timing of the draw is left as it is. Each arrow
//! is made by one call, which takes its start from the player's eye point and its aim from the
//! look: during that call both come from the hands ([`super::aim::with`]).

use super::aim;
use crate::hands;
use crate::model::Model;
use monaka_core::camera::Mat34;
use monaka_core::gesture::{Pull, PullRules};
use monaka_core::protocol::{LEFT_HAND, RIGHT_HAND};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// One build's bow controller code and fields (game DLL RVAs and offsets).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// The controller's update `(controller, dt)`.
    pub update: usize,
    /// One arrow `(controller, target or 0)`.
    pub arrow: usize,
    /// The controller's state (i32); [`DRAWING`] is the draw, [`LOOSING`] the arrow let go.
    pub state: usize,
    /// The draw's power (f32), the most power right after it.
    pub power: usize,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
}

/// The draw state, and the one it goes to as the arrow is let go.
const DRAWING: i32 = 3;
const LOOSING: i32 = 4;
/// Until the string's rest is known: the string hand this far behind the bow hand along its aim
/// (metres) is where the string rests.
const REST: f32 = 0.1;
/// The string pulled 0.45 m back from where it rests is a full draw, and it goes no further back
/// than 0.7 m; any pull counts (a hand in front of the rest draws nothing).
const PULL: PullRules = PullRules { least: 0.0, full: 0.45, longest: 0.7 };
/// A draw the update has not confirmed for this long is over...
const DRAW_FRESH: Duration = Duration::from_millis(200);
/// ...and this long after one the string is back at rest.
const REST_AFTER: Duration = Duration::from_millis(1000);
/// The bow model's string.
const STRING: &str = "bone_string";
/// The hands' places must be this recent (seconds).
const FRESH: f32 = 0.1;
/// The arrow starts this far ahead of the bow hand (metres), past the bow.
const AHEAD_OF_BOW: f32 = 0.1;
/// The bow model's elements the arrow lies along: its nock, else (any element named for the arrow)
/// its root.
const NOCK: &str = "arrow_pose";
/// The bow model's root: it sits on the hand's holder.
const ROOT: i32 = 0;
const ARROW_LIKE: &str = "arrow";
/// The nock must be this far behind the bow it hangs from (metres)...
const NOCK_DEPTH: f32 = 0.05;
/// ...with one of its axes this close to that way (cosine).
const NOCK_ALONG: f32 = 0.8;
/// The holder as the rig placed it must be this recent.
const PLACED_FRESH: Duration = Duration::from_millis(100);
/// The model's elements logged when it is first held.
const LOGGED_ELEMENTS: i32 = 48;

/// What is known of the held bow's model.
struct Known {
    model: usize,
    /// The nock, the axis of it the arrow lies along and which way along it is forward; `None` for a
    /// model without a nock behind its bow.
    arrow: Option<(i32, usize, f32)>,
    string: Option<i32>,
    rest: Option<Rest>,
}

/// The string and the nock at rest (as the game posed them with no draw lately): their matrices
/// against their parent, and where the nock is in the left holder's frame.
#[derive(Clone, Copy)]
struct Rest {
    string: Option<Mat34>,
    nock: Option<Mat34>,
    nock_at: [f32; 3],
}

static KNOWN: Mutex<Option<Known>> = Mutex::new(None);
/// When the player's bow was last seen drawing.
static DRAWN_AT: Mutex<Option<Instant>> = Mutex::new(None);
/// The latest draw's share (f32 bits), for the arrow's line in the log.
static SHARE: AtomicU32 = AtomicU32::new(0);
static STRING_PLACED: AtomicU64 = AtomicU64::new(0);
static BACKWARD_LINES: AtomicU64 = AtomicU64::new(0);
/// The string is drawn back as far as the hand is.
static STRING_FOLLOWS: AtomicBool = AtomicBool::new(true);
/// The bow's line in the left holder's frame: its point level with the holder and its way (unit).
static LINE: Mutex<Option<([f32; 3], [f32; 3])>> = Mutex::new(None);
/// The left holder as the rig last placed it, and when.
static PLACED: Mutex<Option<(Mat34, Instant)>> = Mutex::new(None);
/// The left reticle was put on the bow's line (and must be given back to the palm).
static RETICLE_ON_BOW: AtomicBool = AtomicBool::new(false);

type UpdateFn = unsafe extern "C" fn(usize, f32);
type ArrowFn = unsafe extern "C" fn(usize, usize);

static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
static ARROW_ORIGINAL: Original<ArrowFn> = Original::new();
/// The build, and the player vtable relocated.
static BUILD: OnceLock<(Build, usize)> = OnceLock::new();
static ARROWS: AtomicU64 = AtomicU64::new(0);

/// Hooks `build`'s bow update and arrow in the game DLL (and the aim part, `aim_build`); `string`:
/// the bow model's string drawn back as far as the hand is ([`place_string`]).
pub fn install(hooks: &mut Hooks, gamedll: &Module, build: Build, aim_build: aim::Build, string: bool) -> Result<(), Rejection> {
    aim::install(hooks, gamedll, aim_build)?;
    STRING_FOLLOWS.store(string, Relaxed);
    let _ = BUILD.set((build, gamedll.at(build.player_vtable)));
    // SAFETY: the detours have the targets' signatures (as their code and call sites use them);
    // the game DLL's build is checked by the caller; prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&UPDATE_ORIGINAL, "bow update", gamedll.at(build.update), update as UpdateFn)?;
        hooks.inline_decoded(&ARROW_ORIGINAL, "bow arrow", gamedll.at(build.arrow), arrow as ArrowFn)?;
    }
    Ok(())
}

fn unit(v: [f32; 3]) -> [f32; 3] {
    let l = hands::length(v);
    if l > 1e-9 { v.map(|c| c / l) } else { v }
}

/// `m`'s axes made a rotation (the game's matrices carry a scale), keeping x and the x-y plane.
fn axes(m: &Mat34) -> [[f32; 3]; 3] {
    let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let x = unit([m[0], m[4], m[8]]);
    let z = unit(cross(x, [m[1], m[5], m[9]]));
    [x, cross(z, x), z]
}

/// A world direction in `frame`'s terms.
fn into(frame: &Mat34, v: [f32; 3]) -> [f32; 3] {
    axes(frame).map(|axis| hands::dot(axis, v))
}

/// A world offset in `frame`'s own terms, through its whole 3x3 part (which may scale or mirror, as
/// a model's bones can): `frame` applied to the result gives the offset back.
fn in_terms_of(frame: &Mat34, v: [f32; 3]) -> Option<[f32; 3]> {
    let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let rows = [0, 1, 2].map(|r| [frame[r * 4], frame[r * 4 + 1], frame[r * 4 + 2]]);
    // The inverse's columns are the rows' pairwise crosses over the determinant.
    let columns = [cross(rows[1], rows[2]), cross(rows[2], rows[0]), cross(rows[0], rows[1])];
    let det = hands::dot(rows[0], columns[0]);
    (det.is_finite() && det.abs() > 1e-9).then(|| [0, 1, 2].map(|i| (columns[0][i] * v[0] + columns[1][i] * v[1] + columns[2][i] * v[2]) / det))
}

/// A direction in `frame`'s terms back in the world.
fn out_of(frame: &Mat34, v: [f32; 3]) -> [f32; 3] {
    let [x, y, z] = axes(frame);
    [0, 1, 2].map(|k| x[k] * v[0] + y[k] * v[1] + z[k] * v[2])
}

/// The bow the arms hold (its visual, `bow`): the bow's line from its model, against its root.
pub fn note_bow(bow: usize) {
    if !installed() {
        return;
    }
    let Some(model) = Model::of(bow) else { return };
    let Ok(mut known) = KNOWN.lock() else { return };
    if known.as_ref().is_none_or(|k| k.model != model.id()) {
        *known = Some(first_held(&model));
        if let Ok(mut line) = LINE.lock() {
            *line = None;
        }
    }
    let Some(known) = known.as_mut() else { return };
    let Some((nock, axis, sign)) = known.arrow else { return };
    let (Some(at), Some(grip)) = (model.world(nock), model.world(ROOT)) else { return };
    let way = unit([at[axis], at[4 + axis], at[8 + axis]]).map(|c| c * sign);
    let local_way = into(&grip, way);
    // The line's point level with the root: the root's origin taken onto it.
    let offset = into(&grip, [0, 1, 2].map(|c| at[3 + 4 * c] - grip[3 + 4 * c]));
    let along = hands::dot(offset, local_way);
    let point = [0, 1, 2].map(|c| offset[c] - local_way[c] * along);
    if let Ok(mut line) = LINE.lock() {
        *line = Some((point, local_way));
    }
    // No draw lately: the string and the nock as the game rests them.
    if rested() {
        known.rest = Some(Rest { string: known.string.and_then(|e| model.local(e)), nock: model.local(nock), nock_at: offset });
    }
}

/// A bow model first held: its elements logged and the one the arrow lies along found.
fn first_held(model: &Model) -> Known {
    let root = model.world(ROOT);
    for e in 0..model.count().min(LOGGED_ELEMENTS) {
        let place = model.world(e).zip(root).map(|(m, r)| into(&r, [m[3] - r[3], m[7] - r[7], m[11] - r[11]]));
        log!("bow by hand: bow element [{e}] {} parent {} at {place:.3?} from the root", model.name(e), model.parent(e));
    }
    let nock = model.find(NOCK).or_else(|| model.find_like(ARROW_LIKE));
    let string = model.find(STRING);
    let arrow = nock.and_then(|nock| nock_axis(model, nock).map(|(axis, sign)| (nock, axis, sign)));
    match (nock, arrow) {
        (_, Some((nock, axis, sign))) => log!("bow by hand: the arrow lies along {} [{nock}] axis {axis} ({sign:+}); the string {string:?}", model.name(nock)),
        (Some(nock), None) => log!("bow by hand: {} [{nock}] is not behind the bow it hangs from: the left controller aims", model.name(nock)),
        (None, None) => log!("bow by hand: the bow model has no nock: the left controller aims"),
    }
    Known { model: model.id(), arrow, string, rest: None }
}

/// Which of the nock's axes the arrow lies along and which way along it is forward, from the model
/// alone: the nock sits on the string, behind the bow it hangs from (its parent), so forward runs
/// from the nock to that.
fn nock_axis(model: &Model, nock: i32) -> Option<(usize, f32)> {
    let at = model.world(nock)?;
    let bow = model.world(model.parent(nock))?;
    let forward = [0, 1, 2].map(|k| bow[3 + 4 * k] - at[3 + 4 * k]);
    if hands::length(forward) < NOCK_DEPTH {
        return None;
    }
    let forward = unit(forward);
    let along = [0, 1, 2].map(|c| hands::dot(unit([at[c], at[4 + c], at[8 + c]]), forward));
    let axis = (0..3).max_by(|&a, &b| along[a].abs().total_cmp(&along[b].abs()))?;
    (along[axis].abs() >= NOCK_ALONG).then(|| (axis, along[axis].signum()))
}

fn drawing() -> bool {
    DRAWN_AT.lock().ok().and_then(|d| *d).is_some_and(|at| at.elapsed() < DRAW_FRESH)
}

/// No draw for a while: the string is back where it rests.
fn rested() -> bool {
    DRAWN_AT.lock().ok().and_then(|d| *d).is_none_or(|at| at.elapsed() > REST_AFTER)
}

/// While the bow draws, after the arms were placed: the bow model's string and the arrow's nock
/// pulled straight back along the bow's line from where they rest, as far as the right hand is
/// behind that (the game's draw would pull them all the way, whatever the hand does). The way back
/// is taken in the model as it is now and set against their parent, so it holds wherever the model
/// is put next.
pub fn place_string(bow: usize) {
    if !(installed() && drawing() && STRING_FOLLOWS.load(Relaxed)) {
        return;
    }
    let Some(model) = Model::of(bow) else { return };
    let known = KNOWN.lock().ok().and_then(|k| k.as_ref().filter(|k| k.model == model.id()).map(|k| (k.arrow, k.string, k.rest)));
    let Some((Some((nock, axis, sign)), string, Some(rest))) = known else { return };
    let Some(back) = pulled() else { return };
    let back = back.clamp(0.0, PULL.longest);
    let Some(at) = model.world(nock) else { return };
    let way = unit([at[axis], at[4 + axis], at[8 + axis]]).map(|c| c * sign);
    let mut placed = false;
    for (element, local) in [(string, rest.string), (Some(nock), rest.nock)] {
        let (Some(element), Some(mut local)) = (element, local) else { continue };
        let Some(parent) = model.world(model.parent(element)) else { continue };
        let Some(back_in_parent) = in_terms_of(&parent, way.map(|c| c * back)) else { continue };
        for k in 0..3 {
            local[3 + 4 * k] -= back_in_parent[k];
        }
        placed |= model.set_local(element, &local);
    }
    if placed && STRING_PLACED.fetch_add(1, Relaxed) == 0 {
        log!("bow by hand: the string follows the right hand ({back:.2} m back)");
    }
}

/// The left holder where the rig put it (after the arms were placed, with a bow held): the arrows
/// and the left reticle go along the bow's line from it.
pub fn note_placed(holder: &Mat34) {
    if !installed() {
        return;
    }
    if let Ok(mut placed) = PLACED.lock() {
        *placed = Some((*holder, Instant::now()));
    }
    // The bow's line in the left palm's frame, for the viewer's reticle.
    let along = line().and_then(|(_, way)| {
        let palm = hands::palm(LEFT_HAND, FRESH)?;
        let world = out_of(holder, way);
        let columns = [0, 1, 2].map(|c| [palm[c], palm[4 + c], palm[8 + c]]);
        let local = columns.map(|column| hands::dot(column, world));
        // Ahead of the palm (its -z) only, as the arrows.
        (hands::length(local) > 0.5 && local[2] < 0.0).then_some(local)
    });
    if let Some(local) = along {
        monaka_arms::controllers::aim_reticle(LEFT_HAND, Some(local));
        if !RETICLE_ON_BOW.swap(true, Relaxed) {
            log!("bow by hand: the left reticle follows the bow's line ({local:.3?} in the palm)");
        }
    }
}

/// No bow in hand: the left reticle back along the palm.
pub fn note_no_bow() {
    if RETICLE_ON_BOW.swap(false, Relaxed) {
        monaka_arms::controllers::aim_reticle(LEFT_HAND, None);
    }
}

/// The bow's line in the left holder's frame, once known.
fn line() -> Option<([f32; 3], [f32; 3])> {
    LINE.lock().ok().and_then(|l| *l)
}

/// The bow's place and the arrow's way (unit) now (world): along the bow's line from the holder
/// once known, else along where the left controller points from the palm.
fn bow_now() -> Option<([f32; 3], [f32; 3])> {
    let palm = hands::palm(LEFT_HAND, FRESH)?;
    let pointing = unit(hands::forward(&palm));
    if let (Some((point, way)), Some(holder)) = (line(), placed()) {
        let aim = unit(out_of(&holder, way));
        let offset = out_of(&holder, point);
        // A line pointing back from where the bow hand points is not the bow's.
        if hands::length(aim) > 0.5 && hands::dot(aim, pointing) > 0.0 {
            return Some(([0, 1, 2].map(|k| holder[3 + 4 * k] + offset[k]), aim));
        }
        if BACKWARD_LINES.fetch_add(1, Relaxed) == 0 {
            log!("bow by hand: the bow's line points back from the left controller ({aim:.2?} against {pointing:.2?}); the controller aims instead");
        }
    }
    (hands::length(pointing) > 0.5).then(|| (hands::position(&palm), pointing))
}

/// The left holder where the rig last put it, if lately.
fn placed() -> Option<Mat34> {
    PLACED.lock().ok().and_then(|p| *p).filter(|(_, at)| at.elapsed() < PLACED_FRESH).map(|(m, _)| m)
}

/// How far the string hand is pulled back (metres): behind where the nock rests along the bow's
/// line once that is known, else behind the bow hand less [`REST`].
fn pulled() -> Option<f32> {
    let hand = hands::position(&hands::palm(RIGHT_HAND, FRESH)?);
    let rest = KNOWN.lock().ok().and_then(|k| k.as_ref().and_then(|k| k.rest));
    if let (Some((_, way)), Some(rest), Some(holder)) = (line(), rest, placed()) {
        let at = into(&holder, [0, 1, 2].map(|c| hand[c] - holder[3 + 4 * c]));
        return Some(Pull::back(rest.nock_at, way, at, &PULL));
    }
    let (bow, aim) = bow_now()?;
    Some(hands::dot([0, 1, 2].map(|c| bow[c] - hand[c]), aim) - REST)
}

/// The draw's share (0..1) for the string pulled `back` metres: none in front of the rest.
fn pull(back: f32) -> f32 {
    Pull::share(back, &PULL).unwrap_or(0.0)
}

unsafe extern "C" fn update(controller: usize, dt: f32) {
    let _flight = InFlight::enter();
    let players = BUILD.get().filter(|&&(_, player_vtable)| super::throwing::players(controller, player_vtable)).map(|&(build, _)| build);
    let state = players.and_then(|build| mem::read::<i32>(controller + build.state));
    let share = if state == Some(DRAWING) {
        if let Ok(mut at) = DRAWN_AT.lock() {
            *at = Some(Instant::now());
        }
        pulled().map(pull)
    } else {
        None
    };
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(controller, dt) };
    // The power from the pull (this update's, as the bow looses too), by the game's own curve.
    if let (Some(build), Some(share)) = (players, share)
        && matches!(mem::read::<i32>(controller + build.state), Some(DRAWING | LOOSING))
        && let Some(most) = mem::read::<f32>(controller + build.power + 4).filter(|m| m.is_finite())
    {
        mem::write(controller + build.power, power(share, most));
        SHARE.store(share.to_bits(), Relaxed);
    }
}

/// The game's draw curve: the power for a draw's share (0..1) of a bow whose most power is `most`.
fn power(share: f32, most: f32) -> f32 {
    let left = 1.0 - share.clamp(0.0, 1.0);
    most * (1.0 - left * left)
}

unsafe extern "C" fn arrow(controller: usize, target: usize) {
    let _flight = InFlight::enter();
    let aimed = BUILD
        .get()
        .filter(|(_, player_vtable)| super::throwing::players(controller, *player_vtable))
        .and_then(|_| bow_now())
        .map(|(bow, aim)| ([0, 1, 2].map(|k| bow[k] + aim[k] * AHEAD_OF_BOW), aim));
    match aimed {
        Some((eye, look)) => {
            if ARROWS.fetch_add(1, Relaxed) < 40 {
                let share = f32::from_bits(SHARE.load(Relaxed));
                log!("bow by hand: an arrow along {look:.2?}, drawn {:.0}%{}", share * 100.0, if line().is_some() { "" } else { " (the bow's line not known yet: the left controller's)" });
            }
            // SAFETY: forwards the game's own call, with the hands' aim on this thread.
            aim::with(eye, look, || unsafe { ARROW_ORIGINAL.get()(controller, target) })
        }
        // SAFETY: forwards the game's own call.
        None => unsafe { ARROW_ORIGINAL.get()(controller, target) },
    }
}

/// Whether the bow by hand is on.
pub fn installed() -> bool {
    BUILD.get().is_some() && UPDATE_ORIGINAL.is_set()
}

pub fn report() {
    if BUILD.get().is_some() {
        log!("bow by hand: {} arrows aimed by the hands ({} eye points taken from them), the string placed {} times", ARROWS.load(Relaxed), aim::overridden(), STRING_PLACED.load(Relaxed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_direction_goes_into_a_frame_and_back() {
        // A holder turned a quarter about y (its x along the world's -z): the world's -z is its +x.
        let holder: Mat34 = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0];
        let local = into(&holder, [0.0, 0.0, -1.0]);
        assert!((local[0] - 1.0).abs() < 1e-6 && local[1].abs() < 1e-6 && local[2].abs() < 1e-6, "{local:?}");
        let back = out_of(&holder, local);
        assert!(back[0].abs() < 1e-6 && back[1].abs() < 1e-6 && (back[2] + 1.0).abs() < 1e-6, "{back:?}");
    }

    #[test]
    fn an_offset_goes_into_a_mirrored_scaled_frame() {
        // A frame mirrored in z and scaled by 2: the world's +z is its -0.5 z.
        let frame: Mat34 = [2.0, 0.0, 0.0, 5.0, 0.0, 2.0, 0.0, 6.0, 0.0, 0.0, -2.0, 7.0];
        let local = in_terms_of(&frame, [0.0, 0.0, 1.0]).unwrap();
        assert!(local[0].abs() < 1e-6 && local[1].abs() < 1e-6 && (local[2] + 0.5).abs() < 1e-6, "{local:?}");
        assert!(in_terms_of(&[0.0; 12], [1.0, 0.0, 0.0]).is_none(), "a flat frame has no terms");
    }

    #[test]
    fn the_power_follows_the_games_curve() {
        assert_eq!(power(0.0, 2.0), 0.0);
        assert!((power(0.5, 2.0) - 1.5).abs() < 1e-6);
        assert_eq!(power(1.0, 2.0), 2.0);
        assert_eq!(power(3.0, 2.0), 2.0, "no more than a full draw");
    }

    #[test]
    fn the_pull_runs_from_hands_together_to_a_full_draw() {
        assert_eq!(pull(-0.05), 0.0);
        assert!((pull(PULL.full / 2.0) - 0.5).abs() < 1e-5);
        assert_eq!(pull(0.9), 1.0);
    }
}
