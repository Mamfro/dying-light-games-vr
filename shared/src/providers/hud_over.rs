//! Provider part: the HUD layer's place over each eye (`monaka_stereo::providers::HudLayer::over`),
//! what the Chrome games share.
//!
//! Where a Chrome game's HUD layer ([`crate::providers::hudlayer`]) goes over each eye
//! (`monaka_stereo::hudover12`): in that eye's place at the HUD's distance and size, with that eye's
//! disparity, from the game's frame labels (`monaka_stereo::providers::FrameLabels`). A game
//! provides its HUD layer (`monaka_stereo::providers::HudLayer`) with [`over`], and says the HUD's
//! size and distance when its eyes start ([`set`]), and whether a wheel held open shrinks it
//! ([`shrink_for_wheels`]).

use monaka_core::hud::{Placement, Rect};
use monaka_core::sync::Locked;
use monaka_stereo::hudover12::{CROSS_RADIUS, Params};
use monaka_stereo::providers::FrameLabels;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// The HUD's size and distance while it is laid over the eyes.
static HUD: Mutex<Option<(f32, f32)>> = Mutex::new(None);
/// A wheel held open shows the HUD smaller.
static WHEELS: AtomicBool = AtomicBool::new(true);

/// The HUD's size (1 its size on a monitor) and distance (metres) in front of the eyes while it is
/// laid over them; `None` while it stays as the game draws it.
pub fn set(hud: Option<(f32, f32)>) {
    *HUD.locked() = hud;
}

/// Whether a wheel held open (its d-pad direction held past a tap) shows the HUD smaller while it
/// is open: on unless the game turns it off when its eyes start (its HUD then keeps its size).
pub fn shrink_for_wheels(on: bool) {
    WHEELS.store(on, Relaxed);
}

/// `HudLayer::over` for a Chrome game `G`: how the HUD layer goes over the frame about to present
/// (`size` its pixels), or over its eye `eye` (a feature's image of the frame, such as the frame
/// warped into the other eye): in that eye's place at the HUD's distance and size, with that eye's
/// disparity; a frame no eye camera drew (a menu over a still world) gets it centred, with none.
/// `None` while the eyes are not drawn (the game's own layout then).
pub fn over<G: FrameLabels>(size: (u32, u32), eye: Option<usize>) -> Option<Params> {
    let (scale, distance) = (*HUD.lock().ok()?)?;
    if !G::running() {
        return None;
    }
    // A wheel held open is shown smaller than the HUD (the HUD with it, while it is open).
    let scale = if WHEELS.load(Relaxed) && wheel_open() { scale.min(WHEEL_SCALE) } else { scale };
    place::<G>(size, eye, scale, distance)
}

/// Where [`over`] puts the layer at `scale` and `distance`, from the frame labels alone (whatever
/// the HUD is set to, and whether or not the eyes are drawn). `None` without a frustum to place it
/// in.
pub fn place<G: FrameLabels>(size: (u32, u32), eye: Option<usize>, scale: f32, distance: f32) -> Option<Params> {
    let (frustum, eye, ipd) = match G::presenting() {
        Some(label) => {
            let eye = eye.unwrap_or(label.eye);
            (G::frustum(&label.pose, eye).or_else(G::own_frustum)?, eye, G::separation(&label.pose)?)
        }
        None => (G::own_frustum()?, 0, 0.0),
    };
    let image = Rect::sized(size.0 as f32, size.1 as f32);
    let rect = Placement::new(frustum, eye, ipd, distance).rect(image, image.aspect(), scale).array();
    Some(Params { rect, cross_radius: CROSS_RADIUS * size.1 as f32, ..Default::default() })
}

/// The HUD's size (of the eye's vertical field) while a wheel is held open.
const WHEEL_SCALE: f32 = 0.25;
/// The game opens a wheel on a d-pad press held past a tap (a tap cycles the item instead).
const WHEEL_HOLD_MS: u64 = 210;
/// The d-pad directions that hold a wheel open: the tools' (left) and the weapons' (right).
const WHEEL_BUTTONS: u16 = monaka_core::pad::button::DPAD_LEFT | monaka_core::pad::button::DPAD_RIGHT;

/// Whether a wheel is held open: its d-pad direction held past a tap (the player's, or the
/// viewer's for a hand reached over a shoulder).
fn wheel_open() -> bool {
    static HELD_SINCE: AtomicU64 = AtomicU64::new(0);
    let held = monaka_pad::buttons(250).is_some_and(|b| b & WHEEL_BUTTONS != 0);
    let now = monaka_channel::tick().max(1);
    if !held {
        HELD_SINCE.store(0, Relaxed);
        return false;
    }
    let since = match HELD_SINCE.compare_exchange(0, now, Relaxed, Relaxed) {
        Ok(_) => now,
        Err(since) => since,
    };
    now.saturating_sub(since) >= WHEEL_HOLD_MS
}

#[cfg(test)]
mod tests {
    use super::*;
    use monaka_core::camera::Frustum;
    use monaka_core::protocol::HeadPose;
    use monaka_producer::game::{Adapter, Game};
    use monaka_stereo::driver::Pending;

    /// A Chrome game labelled like The Beast: the frame about to present is one eye of a pose, each
    /// eye drawn with its own field of view, the game camera's own frustum for a frame of no eye.
    struct Chromish;
    static LABEL: Mutex<Option<Pending>> = Mutex::new(None);
    static RUNNING: AtomicBool = AtomicBool::new(false);

    impl Game for Chromish {
        const MANIFESTS: &'static [monaka_core::manifest::Manifest] = &[];
        const ADAPTER: Adapter<Self> = Adapter { components: &[] };
        type Build = ();
        type Frame = ();
        fn prepare(_: &'static monaka_producer::Session, _: &'static monaka_core::manifest::Manifest, _: &monaka_core::options::Options) -> Result<(), monaka_producer::Rejection> {
            Ok(())
        }
    }

    impl FrameLabels for Chromish {
        fn labelled() -> Option<Pending> {
            *LABEL.locked()
        }
        fn presenting() -> Option<Pending> {
            *LABEL.locked()
        }
        fn running() -> bool {
            RUNNING.load(Relaxed)
        }
        fn frustum(pose: &HeadPose, eye: usize) -> Option<Frustum> {
            Frustum::from_fov(pose.fov[eye])
        }
        fn own_frustum() -> Option<Frustum> {
            Frustum::centred([0.9, 0.5])
        }
        fn separation(pose: &HeadPose) -> Option<f32> {
            Some(pose.ipd)
        }
    }

    /// The placement The Beast's HUD layer gets, written out whole (no wheel open): what [`over`]
    /// and [`place`] must keep giving it.
    fn as_before(size: (u32, u32), eye: Option<usize>) -> Option<Params> {
        let (scale, distance) = (*HUD.lock().ok()?)?;
        if !Chromish::running() {
            return None;
        }
        let (frustum, eye, ipd) = match Chromish::presenting() {
            Some(label) => {
                let eye = eye.unwrap_or(label.eye);
                (Chromish::frustum(&label.pose, eye).or_else(Chromish::own_frustum)?, eye, Chromish::separation(&label.pose)?)
            }
            None => (Chromish::own_frustum()?, 0, 0.0),
        };
        let image = Rect::sized(size.0 as f32, size.1 as f32);
        let rect = Placement::new(frustum, eye, ipd, distance).rect(image, image.aspect(), scale).array();
        Some(Params { rect, cross_radius: CROSS_RADIUS * size.1 as f32, ..Default::default() })
    }

    fn same(a: Option<Params>, b: Option<Params>) -> bool {
        match (a, b) {
            (Some(a), Some(b)) => a.rect == b.rect && a.aim == b.aim && a.cross_radius == b.cross_radius && a.piece_count == b.piece_count,
            (None, None) => true,
            _ => false,
        }
    }

    #[test]
    fn the_layer_goes_where_it_went() {
        let pose = HeadPose { valid: 1, ipd: 0.063, fov: [[-0.9, 0.8, 0.85, -0.8], [-0.8, 0.9, 0.85, -0.8]], ..HeadPose::default() };
        let size = (2016, 2240);
        let cases = || {
            let mut cases = Vec::new();
            for eye in [None, Some(0), Some(1)] {
                cases.push((eye, over::<Chromish>(size, eye), as_before(size, eye)));
            }
            cases
        };
        // Not laid over yet; laid over but the eyes not drawn.
        assert!(cases().iter().all(|(_, new, old)| new.is_none() && old.is_none()));
        set(Some((0.55, 1.4)));
        assert!(cases().iter().all(|(_, new, old)| new.is_none() && old.is_none()));
        RUNNING.store(true, Relaxed);
        // Each eye of a labelled frame, a frame of no eye, an eye without a usable field of view.
        let mut flat = pose;
        flat.fov[1] = [0.0; 4];
        for label in [Some(Pending { eye: 0, pose }), Some(Pending { eye: 1, pose }), None, Some(Pending { eye: 1, pose: flat })] {
            *LABEL.locked() = label;
            for (eye, new, old) in cases() {
                assert!(new.is_some(), "placed: {eye:?}");
                assert!(same(new, old), "{eye:?}: {new:?} against {old:?}");
            }
        }
        // The eyes apart by their disparity, and the placement the same as `place`'s.
        *LABEL.locked() = Some(Pending { eye: 0, pose });
        let (left, right) = (over::<Chromish>(size, Some(0)).unwrap(), over::<Chromish>(size, Some(1)).unwrap());
        assert!(left.rect[0] > right.rect[0]);
        assert!(same(over::<Chromish>(size, None), place::<Chromish>(size, None, 0.55, 1.4)));
        // Turned off, a wheel no longer shrinks it (none is open here either way).
        shrink_for_wheels(false);
        assert!(same(over::<Chromish>(size, Some(1)), as_before(size, Some(1))));
        shrink_for_wheels(true);
        set(None);
        RUNNING.store(false, Relaxed);
    }
}
