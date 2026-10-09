//! The first-person arms follow the controllers (`hand_rig`): around the arms visual's
//! camera-target callback (`engine::FPP_CAMERA_TARGET`) the shared Chrome rig
//! ([`eng_chr::fpp::around`]) snapshots the arms before the game squashes them for the flat
//! screen, then puts them on the controllers, feeds the swings and places tracked fingers. What
//! is Dying Light 1's own: its arms skeleton is the visual's `IModelObject`, reached through the
//! engine's element exports ([`Model`]; the rest pose read by resetting a hand to its reference
//! frame), the camera is mapped back to the game's when the present thread turned it first
//! ([`camera_matrix`]), and its aim and tracking origin are its own (`aim`, `stereo`). Hand aim
//! points the game camera where the held weapon points ([`monaka_core::math::converged`]).
//!
//! Its probes (`probe_hands`, `probe_fingers`, `probe_rig`) are in `research::arms`.

use crate::engine::{self, Camera, CameraTargetFn};
use crate::view::stereo;
use eng_chr::fpp::{self, ArmsSkeleton};
use monaka_arms::Skeleton;
use monaka_core::camera::Mat34;
use monaka_core::protocol::{LEFT_HAND, RIGHT_HAND};
use monaka_hook::module::Module;
use monaka_hook::probe::class_name;
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::sync::{Mutex, OnceLock};

pub static CAMERA_TARGET_ORIGINAL: Original<CameraTargetFn> = Original::new();

const LAYOUT: fpp::Layout = fpp::Layout { weapons: engine::FPP_VIS_WEAPONS };
const FINGER_AXES: eng_chr::fingers::Axes = eng_chr::fingers::Axes::DYING_LIGHT_1;

/// The engine's element accessors.
struct Elements {
    number: engine::ElementsNumberFn,
    name: engine::ElementNameFn,
    parent: engine::ElementParentFn,
    world: engine::ElementWorldFn,
    set_world: engine::SetElementWorldFn,
    is_bone: engine::ElementIsBoneFn,
    /// For the rest pose (fingers) and its probe.
    reset_descendants: Option<engine::ResetDescendantsFn>,
    local: Option<engine::ElementLocalFn>,
    set_local: Option<engine::SetElementLocalFn>,
}

static ELEMENTS: OnceLock<Elements> = OnceLock::new();
/// More elements than this is not the arms model.
const MAX_ELEMENTS: i32 = 512;

/// Looks up the engine's element accessors; the hook is installed only when they all exist.
pub fn resolve(engine_module: &Module) -> Result<(), Rejection> {
    let find = |name: &str| engine_module.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    // SAFETY: each export has the signature its mangled name states, on x64.
    let elements = unsafe {
        Elements {
            number: std::mem::transmute::<usize, engine::ElementsNumberFn>(find(engine::ELEMENTS_NUMBER)?),
            name: std::mem::transmute::<usize, engine::ElementNameFn>(find(engine::ELEMENT_NAME)?),
            parent: std::mem::transmute::<usize, engine::ElementParentFn>(find(engine::ELEMENT_PARENT)?),
            world: std::mem::transmute::<usize, engine::ElementWorldFn>(find(engine::ELEMENT_WORLD)?),
            set_world: std::mem::transmute::<usize, engine::SetElementWorldFn>(find(engine::SET_ELEMENT_WORLD)?),
            is_bone: std::mem::transmute::<usize, engine::ElementIsBoneFn>(find(engine::ELEMENT_IS_BONE)?),
            reset_descendants: engine_module.export(engine::RESET_DESCENDANTS).map(|f| std::mem::transmute::<usize, engine::ResetDescendantsFn>(f)),
            local: engine_module.export(engine::ELEMENT_LOCAL).map(|f| std::mem::transmute::<usize, engine::ElementLocalFn>(f)),
            set_local: engine_module.export(engine::SET_ELEMENT_LOCAL).map(|f| std::mem::transmute::<usize, engine::SetElementLocalFn>(f)),
        }
    };
    let _ = ELEMENTS.set(elements);
    Ok(())
}

/// The arms vis's vtable, once its class name has been checked.
static VIS_VTABLE: AtomicU64 = AtomicU64::new(0);

fn is_arms(vis: usize) -> bool {
    let Some(vtable) = mem::read::<usize>(vis) else { return false };
    let known = VIS_VTABLE.load(Relaxed);
    if known == 0 && class_name(vis).as_deref() == Some(engine::FPP_VIS_CLASS) {
        VIS_VTABLE.store(vtable as u64, Relaxed);
        return true;
    }
    known != 0 && known == vtable as u64
}

/// The arms model, asked from the vis's model holder the way the callback itself asks.
fn model(vis: usize) -> Option<usize> {
    let holder = vis + engine::FPP_VIS_MODEL_HOLDER;
    let get = mem::read::<usize>(mem::read::<usize>(holder)? + engine::MODEL_HOLDER_GET_MODEL)?;
    // Only code of the game DLL: anything else means the layout is not what was measured.
    if !Module::find(engine::GAMEDLL).is_some_and(|m| m.contains(get)) {
        return None;
    }
    // SAFETY: the holder's own virtual, `IModelObject* (FakeModelObject*)`, called on the game
    // thread right after the game's own calls to it.
    let model = unsafe { std::mem::transmute::<usize, unsafe extern "system" fn(*mut c_void) -> *mut c_void>(get)(holder as *mut c_void) };
    (!model.is_null()).then_some(model as usize)
}

/// Calls that were handed the camera already turned to the view.
static UNTURNED: AtomicU64 = AtomicU64::new(0);

/// The camera's camera-to-world matrix as the game set it, if the interface and its state point
/// at each other. The present thread turns the player camera to the view, and once in a few
/// hundred updates that turn lands between the game setting the camera and calling the arms: the
/// arms were then placed against the head, a one-frame jump of the gun in one eye (measured
/// 2026-10-06, `probe_rig`). Such a camera is mapped back to the game's ([`stereo::game_camera`]).
pub(crate) fn camera_matrix(camera: usize) -> Option<Mat34> {
    let state = mem::read::<usize>(camera + engine::INTERFACE_STATE)?;
    (mem::read::<usize>(state + engine::STATE_INTERFACE)? == camera).then_some(())?;
    let read = Camera::read(state)?.inverse;
    let game = stereo::game_camera(&read);
    if game != read {
        UNTURNED.fetch_add(1, Relaxed);
    }
    Some(game)
}

/// The arms visual's `IModelObject`, through the engine's element exports.
pub(crate) struct Model<'a> {
    elements: &'a Elements,
    model: *mut c_void,
    count: i32,
}

impl<'a> Model<'a> {
    fn new(elements: &'a Elements, model: usize) -> Option<Self> {
        let model = model as *mut c_void;
        // SAFETY: a live IModelObject from the vis, on the game thread.
        let count = unsafe { (elements.number)(model) };
        (1..=MAX_ELEMENTS).contains(&count).then_some(Self { elements, model, count })
    }

    /// The vis's arms model, or why not.
    pub(crate) fn of(vis: usize) -> Result<Model<'static>, String> {
        let elements = ELEMENTS.get().ok_or("no element functions")?;
        let model = model(vis).ok_or("no arms model")?;
        let model = Model::new(elements, model).ok_or_else(|| format!("arms model {model:#x} has no plausible element count"))?;
        ARMS_MODEL.store(model.id() as u64, Relaxed);
        Ok(model)
    }

    /// Element `e`'s local matrix, through the engine (when it exports the access).
    pub(crate) fn local(&self, e: i32) -> Option<Mat34> {
        let local = self.elements.local?;
        // SAFETY: a valid element index; the engine returns a reference to its matrix.
        mem::read::<Mat34>(unsafe { local(self.model, e) } as usize)
    }

    /// `under`'s local matrices, then `read` with `hand`'s descendants reset to the reference frame
    /// (the rest pose), then the local matrices put back.
    pub(crate) fn at_rest<R>(&self, hand: i32, under: &[i32], read: impl FnOnce(&Self) -> R) -> Option<R> {
        let (reset, set_local) = (self.elements.reset_descendants?, self.elements.set_local?);
        let saved: Vec<(i32, Option<Mat34>)> = under.iter().map(|&e| (e, self.local(e))).collect();
        // SAFETY: the engine's own reset, on the game thread, for an element of this model.
        unsafe { reset(self.model, hand) };
        let result = read(self);
        for (e, matrix) in saved {
            if let Some(matrix) = matrix {
                let aligned = monaka_core::Aligned16(matrix);
                // SAFETY: a valid element index and a matrix that lives across the call.
                unsafe { set_local(self.model, e, aligned.0.as_ptr()) };
            }
        }
        Some(result)
    }

    /// `hand`'s descendants, parents first.
    pub(crate) fn under(&self, hand: i32) -> Vec<i32> {
        let mut under = vec![hand];
        let mut k = 0;
        while k < under.len() {
            let parent = under[k];
            under.extend((0..self.count).filter(|&e| self.parent(e) == parent));
            k += 1;
        }
        under
    }
}

impl Skeleton for Model<'_> {
    fn id(&self) -> usize {
        self.model as usize
    }

    fn count(&self) -> i32 {
        self.count
    }

    fn world(&self, element: i32) -> Option<Mat34> {
        // SAFETY: a valid element index; the engine returns a reference to its matrix (or to a
        // static identity for a bad index).
        let matrix = unsafe { (self.elements.world)(self.model, element) };
        mem::read::<Mat34>(matrix as usize)
    }

    fn set_world(&self, element: i32, matrix: &Mat34) {
        // SAFETY: a valid element index and a 12-float matrix that lives across the call.
        unsafe { (self.elements.set_world)(self.model, element, matrix.as_ptr()) }
    }

    fn name(&self, element: i32) -> String {
        // SAFETY: a valid element index; the engine returns its name or null.
        let text = unsafe { (self.elements.name)(self.model, element) };
        mem::read_c_string(text as usize, 64).unwrap_or_default()
    }

    fn is_bone(&self, element: i32) -> bool {
        // SAFETY: a valid element index; a plain lookup despite the non-const signature.
        unsafe { (self.elements.is_bone)(self.model, element) }
    }

    fn parent(&self, element: i32) -> i32 {
        // SAFETY: a valid element index.
        unsafe { (self.elements.parent)(self.model, element) }
    }
}

impl ArmsSkeleton for Model<'_> {
    /// The engine keeps no rest pose apart: the hand's descendants are reset to the reference
    /// frame, read and put back.
    fn rest_poses(&self, hand: i32, bones: &[i32]) -> Option<Vec<(i32, Mat34)>> {
        let under = self.under(hand);
        self.at_rest(hand, &under, |model| bones.iter().filter_map(|&bone| model.local(bone).map(|m| (bone, m))).collect())
    }
}

/// The arms model the rig last looked up (0 before the first).
static ARMS_MODEL: AtomicU64 = AtomicU64::new(0);

/// The arms model the rig last looked up, for probes comparing it with the game's own reads.
pub fn arms_model() -> usize {
    ARMS_MODEL.load(Relaxed) as usize
}

/// Arms callbacks so far (the rig poses the arms once per callback).
pub fn rig_calls() -> u64 {
    fpp::calls()
}

/// `PlayerFppVis`'s camera-target callback `(vis, camera)`.
pub unsafe extern "system" fn camera_target(vis: *mut c_void, camera: *mut c_void) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let original = || unsafe { CAMERA_TARGET_ORIGINAL.get()(vis, camera) };
    let (vis, camera) = (vis as usize, camera as usize);
    if !(stereo::capturing() && is_arms(vis)) {
        original();
        return;
    }
    let config = stereo::config();
    let palms = [stereo::pending_palm_of(LEFT_HAND), stereo::pending_palm_of(RIGHT_HAND)];
    let options = fpp::Options {
        rig: config.aim.rig,
        probe: crate::research::options().hands,
        melee: config.melee,
        fingers: (config.aim.rig && config.fingers.tracking).then_some(FINGER_AXES),
        palms,
    };
    let game = camera_matrix(camera);
    let pose = |game: Mat34, gun: bool| -> Result<monaka_arms::Pose, &'static str> {
        if !crate::player::aim::live() {
            return Err("head aim not steering");
        }
        // The same tracking origin the view is drawn from, so a hand lands where its controller is seen.
        let origin = stereo::tracking_origin(&game, true, config.aim.hand);
        Ok(monaka_arms::Pose {
            game,
            origin,
            head: stereo::pending_pose(),
            palms,
            aim_side: config.aim.side,
            hold: config.hand_hold,
            gun,
            melee_holder: None,
            bones: monaka_arms::BoneAxes::DYING_LIGHT_1,
        })
    };
    let outcome = fpp::around(vis, &LAYOUT, &options, Model::of, game, original, pose);
    crate::research::arms::moved(outcome.placed.as_ref(), game);
    crate::research::arms::after(vis, camera, outcome.before.as_ref());
    note_frame();
}

/// Callbacks per presented frame, by the eye that frame renders: a frame with none was drawn with
/// whatever the game's own update left in the arms.
struct FrameTally {
    present: u64,
    calls: u64,
    eye: i32,
    /// [eye][0, 1, 2, 3+ calls]
    counts: [[u64; 4]; 2],
}

static FRAMES: Mutex<FrameTally> = Mutex::new(FrameTally { present: 0, calls: 0, eye: -1, counts: [[0; 4]; 2] });

fn note_frame() {
    let Some(present) = stereo::drawing_present() else { return };
    let Ok(mut t) = FRAMES.lock() else { return };
    if present != t.present {
        if t.present != 0 && (0..2).contains(&t.eye) {
            let eye = t.eye as usize;
            let bucket = t.calls.min(3) as usize;
            t.counts[eye][bucket] += 1;
            // Frames in between had no callback at all; their eyes alternate.
            for skipped in t.present + 1..present.min(t.present + 64) {
                let e = (eye + (skipped - t.present) as usize) % 2;
                t.counts[e][0] += 1;
            }
        }
        t.present = present;
        t.calls = 0;
        t.eye = stereo::eye();
    }
    t.calls += 1;
}

/// End-of-run tally.
pub fn report() {
    fpp::report();
    let unturned = UNTURNED.load(Relaxed);
    if unturned > 0 {
        log!("  handed the camera already turned to the view {unturned}x (mapped back)");
    }
    if let Ok(t) = FRAMES.lock() {
        for (eye, counts) in t.counts.iter().enumerate() {
            log!("  frames for eye {eye} by arms callbacks [0, 1, 2, 3+]: {counts:?}");
        }
    }
}
