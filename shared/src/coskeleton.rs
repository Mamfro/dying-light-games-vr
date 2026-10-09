//! The arms skeleton of Dying Light 2 and The Beast: a `CoSkeleton` component, reached from the
//! first-person arms visual by a component handle, through the engine's component pool. Its
//! element access is exported by the engine (virtuals, called directly once the component's own
//! vtable is seen to hold the same functions). Element type 4 is a bone (3 a helper), as the
//! engine's `IModelObject::IsElementABone` tests (confirmed in DL2: 364 bones, 26 helpers).

use crate::fpp::ArmsSkeleton;
use monaka_arms::Skeleton;
use monaka_core::camera::Mat34;
use monaka_hook::mem;
use monaka_hook::module::Module;
use monaka_hook::probe::class_name;
use monaka_producer::Rejection;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::*};

pub const COMPONENTS_POOL: &str = "?GetComponentsPool@cbs@@YAAEAVComponentsPool@1@XZ";
pub const GET_COMPONENT: &str = "?GetComponentImpl@ComponentsPool@cbs@@QEAAPEAVCBaseObject@2@W4Handle@detail@2@@Z";
pub const COUNT: &str = "?GetNumElements@CoSkeleton@@UEBAIXZ";
pub const NAME: &str = "?GetElementName@CoSkeleton@@UEBA?AV?$string_const@D@ttl@@H@Z";
pub const PARENT: &str = "?GetParentElementIndex@CoSkeleton@@UEBAHH@Z";
pub const WORLD: &str = "?GetWorldXformElement@CoSkeleton@@UEBA?AVmtx34@@H@Z";
pub const SET_WORLD: &str = "?SetWorldXformElement@CoSkeleton@@UEAAXHAEBVmtx34@@@Z";
pub const TYPE: &str = "?GetElementType@CoSkeleton@@UEBA?AW4EntityID@@H@Z";
/// An element's reference frame (its rest pose) against its parent; Dying Light 2 exports it
/// (finger tracking reads the rest pose from it). Optional: a build without it has no fingers.
pub const REFERENCE_LOCAL: &str = "?GetElementReferenceLocalXform@CoSkeleton@@UEBA?AVmtx34@@H@Z";
pub const TYPE_BONE: i32 = 4;

type PoolFn = unsafe extern "system" fn() -> usize;
type ComponentFn = unsafe extern "system" fn(pool: usize, handle: u64) -> usize;
type CountFn = unsafe extern "system" fn(skeleton: usize) -> u32;
/// Returns `out`, a `string_const` whose first field is the C string's address (tagged).
type NameFn = unsafe extern "system" fn(skeleton: usize, out: *mut [usize; 4], element: i32) -> *mut [usize; 4];
type ParentFn = unsafe extern "system" fn(skeleton: usize, element: i32) -> i32;
/// Returns `out`.
type WorldFn = unsafe extern "system" fn(skeleton: usize, out: *mut f32, element: i32) -> *mut f32;
type SetWorldFn = unsafe extern "system" fn(skeleton: usize, element: i32, matrix: *const f32);
type TypeFn = unsafe extern "system" fn(skeleton: usize, element: i32) -> i32;

/// Where a game keeps the skeleton: the handle's offset in the arms visual, and the `CoSkeleton`
/// vtable offsets of its world, count, name and set-world functions (checked against the exports).
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub handle: usize,
    pub world_slot: usize,
    pub count_slot: usize,
    pub name_slot: usize,
    pub set_world_slot: usize,
}

struct Functions {
    pool: PoolFn,
    component: ComponentFn,
    count: CountFn,
    name: NameFn,
    parent: ParentFn,
    world: WorldFn,
    set_world: SetWorldFn,
    kind: TypeFn,
    /// Returns `out`: the element's rest pose against its parent ([`REFERENCE_LOCAL`]).
    reference_local: Option<WorldFn>,
    /// (vtable offset, export) pairs that must agree on a live skeleton.
    slots: [(usize, usize); 4],
    handle: usize,
}

static FUNCTIONS: OnceLock<Functions> = OnceLock::new();
/// More elements than this is not the arms skeleton.
const MAX_ELEMENTS: u32 = 512;

/// Looks up the engine's skeleton access; arms hooks are installed only when it all exists.
pub fn resolve(engine: &Module, layout: &Layout) -> Result<(), Rejection> {
    let find = |name: &str| engine.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    let (world, count, name, set_world) = (find(WORLD)?, find(COUNT)?, find(NAME)?, find(SET_WORLD)?);
    // SAFETY: each export has the signature its mangled name states, on x64 (by-value results
    // through a hidden pointer after `this`).
    let functions = unsafe {
        Functions {
            pool: std::mem::transmute::<usize, PoolFn>(find(COMPONENTS_POOL)?),
            component: std::mem::transmute::<usize, ComponentFn>(find(GET_COMPONENT)?),
            count: std::mem::transmute::<usize, CountFn>(count),
            name: std::mem::transmute::<usize, NameFn>(name),
            parent: std::mem::transmute::<usize, ParentFn>(find(PARENT)?),
            world: std::mem::transmute::<usize, WorldFn>(world),
            set_world: std::mem::transmute::<usize, SetWorldFn>(set_world),
            kind: std::mem::transmute::<usize, TypeFn>(find(TYPE)?),
            reference_local: engine.export(REFERENCE_LOCAL).map(|f| std::mem::transmute::<usize, WorldFn>(f)),
            slots: [(layout.world_slot, world), (layout.count_slot, count), (layout.name_slot, name), (layout.set_world_slot, set_world)],
            handle: layout.handle,
        }
    };
    let _ = FUNCTIONS.set(functions);
    Ok(())
}


/// The arms' `CoSkeleton`.
pub struct CoSkeleton {
    functions: &'static Functions,
    skeleton: usize,
    count: i32,
}

/// The skeleton vtable once its slots were checked against the exports.
static CHECKED_VTABLE: AtomicU64 = AtomicU64::new(0);

impl CoSkeleton {
    /// The arms visual `vis`'s skeleton, through its component handle, or why not.
    pub fn of(vis: usize) -> Result<Self, String> {
        let functions = FUNCTIONS.get().ok_or("no skeleton functions")?;
        let handle = mem::read::<u64>(vis + functions.handle).filter(|&h| h != 0).ok_or("no skeleton handle")?;
        // SAFETY: the engine's own pool lookup, on the game thread, with a handle the vis holds;
        // it returns null for a stale handle.
        let skeleton = unsafe { (functions.component)((functions.pool)(), handle) };
        if skeleton == 0 {
            return Err(format!("skeleton handle {handle:#x} resolves to nothing"));
        }
        let vtable = mem::read::<usize>(skeleton).ok_or("unreadable skeleton")?;
        if CHECKED_VTABLE.load(Relaxed) != vtable as u64 {
            if let Some((slot, _)) = functions.slots.iter().find(|&&(slot, export)| mem::read::<usize>(vtable + slot) != Some(export)) {
                return Err(format!("the arms component's vtable slot {slot:#x} is not CoSkeleton's export ({})", class_name(skeleton).unwrap_or_default()));
            }
            CHECKED_VTABLE.store(vtable as u64, Relaxed);
        }
        // SAFETY: a live CoSkeleton (vtable checked), on the game thread.
        let count = unsafe { (functions.count)(skeleton) };
        if !(1..=MAX_ELEMENTS).contains(&count) {
            return Err(format!("skeleton {skeleton:#x} has {count} elements"));
        }
        Ok(Self { functions, skeleton, count: count as i32 })
    }

    pub fn kind(&self, element: i32) -> i32 {
        // SAFETY: a valid element index.
        unsafe { (self.functions.kind)(self.skeleton, element) }
    }

    /// Element `element`'s rest pose against its parent (the skeleton's reference frame), where
    /// the engine exports it.
    fn reference_local(&self, element: i32) -> Option<Mat34> {
        let reference = self.functions.reference_local?;
        let mut out = monaka_core::Aligned16([0.0; 12]);
        // SAFETY: a valid element index; the matrix is written into `out` (returned by value
        // through the hidden pointer, as the mangled name states).
        unsafe { reference(self.skeleton, out.0.as_mut_ptr(), element) };
        out.0.iter().all(|v| v.is_finite()).then_some(out.0)
    }
}

impl ArmsSkeleton for CoSkeleton {
    fn rest_poses(&self, _hand: i32, bones: &[i32]) -> Option<Vec<(i32, Mat34)>> {
        self.functions.reference_local?;
        Some(bones.iter().filter_map(|&bone| self.reference_local(bone).map(|m| (bone, m))).collect())
    }

    fn kind_label(&self, element: i32) -> String {
        format!("type {}", self.kind(element))
    }
}

impl Skeleton for CoSkeleton {
    fn id(&self) -> usize {
        self.skeleton
    }

    fn count(&self) -> i32 {
        self.count
    }

    fn name(&self, element: i32) -> String {
        let mut out = [0usize; 4];
        // SAFETY: a valid element index; the result is written into `out` (a `string_const`,
        // C string first).
        unsafe { (self.functions.name)(self.skeleton, &mut out, element) };
        // A tagged pointer: The Beast keeps flags in the top byte (it returns the C string's address
        // or-ed with 0x11 << 56); Dying Light 2's are clear there.
        mem::read_c_string(out[0] & 0x00ff_ffff_ffff_ffff, 64).unwrap_or_default()
    }

    fn parent(&self, element: i32) -> i32 {
        // SAFETY: a valid element index.
        unsafe { (self.functions.parent)(self.skeleton, element) }
    }

    fn world(&self, element: i32) -> Option<Mat34> {
        let mut out = monaka_core::Aligned16([0.0; 12]);
        // SAFETY: a valid element index; the matrix is written into `out`.
        unsafe { (self.functions.world)(self.skeleton, out.0.as_mut_ptr(), element) };
        out.0.iter().all(|v| v.is_finite()).then_some(out.0)
    }

    fn set_world(&self, element: i32, matrix: &Mat34) {
        let aligned = monaka_core::Aligned16(*matrix);
        // SAFETY: a valid element index and an aligned matrix that lives across the call.
        unsafe { (self.functions.set_world)(self.skeleton, element, aligned.0.as_ptr()) }
    }

    fn is_bone(&self, element: i32) -> bool {
        self.kind(element) == TYPE_BONE
    }
}
