//! Provider: a held item's model (`monaka_body::held::HeldItems::Model`), its elements read and
//! written.
//!
//! A model visual's elements (a held weapon's visual is an `IModelObject` at its start, as the bow's
//! `BowVis` is; Dying Light 1's arms visual reaches its arms model the same way), read and written
//! through the engine's element exports ([`Exports`], looked up once by [`resolve`]). Where those are
//! virtuals (Dying Light 2, The Beast), a model is read through them only when its vtable holds
//! each one (its class does not override it), and written only when it also holds the writes; a
//! vtable in the game DLL reaches them through a jump to the import. Dying Light 1's are plain
//! functions, good for every model. A local matrix set stays across the engine's next update of the
//! model (it marks the model for one), which works the world matrices out from the locals.

use crate::providers::fpp::ArmsSkeleton;
use monaka_body::Skeleton;
use monaka_core::camera::Mat34;
use monaka_hook::mem;
use monaka_hook::module::Module;
use monaka_producer::log;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// How a build's engine exports `IModelObject`'s element functions: their mangled names.
#[derive(Clone, Copy, Debug)]
pub struct Exports {
    pub count: &'static str,
    pub name: &'static str,
    pub parent: &'static str,
    pub world: &'static str,
    /// The local matrix's read and write.
    pub local: &'static str,
    pub set_local: &'static str,
    /// For posing a model's bones: the world matrix's write, the bone test, and a hand's
    /// descendants reset to the reference frame (the rest pose).
    pub set_world: &'static str,
    pub is_bone: &'static str,
    pub reset_descendants: &'static str,
    /// The element functions are virtuals: a class may override them.
    pub virtuals: bool,
}

/// Dying Light 2's and The Beast's exports: virtuals (the reset is a plain function).
pub const VIRTUAL: Exports = Exports {
    count: "?GetElementsNumber@IModelObject@@UEBAHXZ",
    name: "?GetElementNameCStr@IModelObject@@UEBAPEBDH@Z",
    parent: "?GetElementParent@IModelObject@@UEBAHH@Z",
    world: "?GetElementWorldMatrix@IModelObject@@UEBAAEBVmtx34@@H@Z",
    local: "?GetElementLocalMatrix@IModelObject@@UEBAAEBVmtx34@@H@Z",
    set_local: "?SetElementLocalMatrix@IModelObject@@UEAAXHAEBVmtx34@@@Z",
    set_world: "?SetElementWorldMatrix@IModelObject@@UEAAXHAEBVmtx34@@@Z",
    is_bone: "?IsElementABone@IModelObject@@UEAA_NH@Z",
    reset_descendants: "?ResetElementsDescendantsToReferenceFrame@IModelObject@@QEAAXH@Z",
    virtuals: true,
};

/// More elements than this is not a weapon's or the arms' model.
const MOST_ELEMENTS: i32 = 512;
/// How many vtable slots are searched for the exports.
const VTABLE_SLOTS: usize = 600;

type CountFn = unsafe extern "system" fn(usize) -> i32;
/// Returns the name (a C string) or null.
type NameFn = unsafe extern "system" fn(usize, i32) -> usize;
type ParentFn = unsafe extern "system" fn(usize, i32) -> i32;
/// Returns a reference to the element's matrix.
type MatrixFn = unsafe extern "system" fn(usize, i32) -> usize;
type SetMatrixFn = unsafe extern "system" fn(usize, i32, *const f32);
type IsBoneFn = unsafe extern "system" fn(usize, i32) -> bool;
type ResetFn = unsafe extern "system" fn(usize, i32);

struct Functions {
    count: usize,
    name: usize,
    parent: usize,
    world: usize,
    /// The local matrix's read and write, where exported.
    local: Option<(usize, usize)>,
    /// The world matrix's write and the bone test, where exported.
    pose: Option<(usize, usize)>,
    reset: Option<usize>,
    virtuals: bool,
}

static FUNCTIONS: OnceLock<Functions> = OnceLock::new();

/// Looks up the engine's element functions by `exports`; why not, when it lacks one of the reads
/// (the local matrix, the posing functions and the reset are each optional). Once per run.
pub fn resolve(engine: &Module, exports: &Exports) -> Result<(), String> {
    if FUNCTIONS.get().is_some() {
        return Ok(());
    }
    let find = |name: &str| engine.export(name).ok_or_else(|| format!("the engine does not export {name}"));
    let functions = Functions {
        count: find(exports.count)?,
        name: find(exports.name)?,
        parent: find(exports.parent)?,
        world: find(exports.world)?,
        local: engine.export(exports.local).zip(engine.export(exports.set_local)),
        pose: engine.export(exports.set_world).zip(engine.export(exports.is_bone)),
        reset: engine.export(exports.reset_descendants),
        virtuals: exports.virtuals,
    };
    let _ = FUNCTIONS.set(functions);
    Ok(())
}

/// Where the code at `at` goes: through a jump to an import (`jmp [rip+n]`) or a plain jump
/// (`jmp rel32`), else `at` itself.
fn through_jumps(at: usize) -> usize {
    let mut at = at;
    for _ in 0..2 {
        let Some(bytes) = mem::read::<[u8; 7]>(at) else { break };
        let rel = |from: usize| i32::from_le_bytes([bytes[from], bytes[from + 1], bytes[from + 2], bytes[from + 3]]) as isize;
        at = match bytes {
            [0xff, 0x25, ..] => match mem::read::<usize>(at.wrapping_add_signed(6 + rel(2))) {
                Some(target) => target,
                None => break,
            },
            [0x48, 0xff, 0x25, ..] => match mem::read::<usize>(at.wrapping_add_signed(7 + rel(3))) {
                Some(target) => target,
                None => break,
            },
            [0xe9, ..] => at.wrapping_add_signed(5 + rel(1)),
            _ => break,
        };
    }
    at
}

/// The vtable last seen to hold a set of functions, and the last one seen not to.
struct Verdicts {
    holds: AtomicU64,
    lacks: AtomicU64,
}

static READ: Verdicts = Verdicts { holds: AtomicU64::new(0), lacks: AtomicU64::new(0) };
static WRITE: Verdicts = Verdicts { holds: AtomicU64::new(0), lacks: AtomicU64::new(0) };
static POSE: Verdicts = Verdicts { holds: AtomicU64::new(0), lacks: AtomicU64::new(0) };

/// Whether `object`'s class keeps each of the engine's `wanted` functions (checked once per vtable,
/// the verdict kept in `verdicts`); always, where they are plain functions.
fn inherits(functions: &Functions, object: usize, wanted: &[(usize, &str)], verdicts: &Verdicts) -> bool {
    if !functions.virtuals {
        return true;
    }
    let Some(vtable) = mem::read::<usize>(object).filter(|&v| v != 0) else { return false };
    if verdicts.holds.load(Relaxed) == vtable as u64 {
        return true;
    }
    if verdicts.lacks.load(Relaxed) == vtable as u64 {
        return false;
    }
    let slots: Vec<usize> = (0..VTABLE_SLOTS).map_while(|k| mem::read::<usize>(vtable + k * 8)).map(through_jumps).collect();
    let missing: Vec<&str> = wanted.iter().filter(|(f, _)| !slots.contains(f)).map(|&(_, what)| what).collect();
    if missing.is_empty() {
        verdicts.holds.store(vtable as u64, Relaxed);
        return true;
    }
    verdicts.lacks.store(vtable as u64, Relaxed);
    log!("model elements: {} overrides the engine's {} (left alone)", monaka_hook::probe::class_name(object).unwrap_or_default(), missing.join(", "));
    false
}

/// A model's elements (the [`Skeleton`] reads and writes, and the rest pose for the arms).
pub struct Model {
    object: usize,
    functions: &'static Functions,
    count: i32,
}

impl Model {
    /// The model at the start of the visual `object`, when its class keeps the engine's element
    /// functions and its element count is plausible.
    pub fn of(object: usize) -> Option<Self> {
        let functions = FUNCTIONS.get()?;
        let wanted = [(functions.count, "count"), (functions.name, "name"), (functions.parent, "parent"), (functions.world, "world matrix")];
        if object == 0 || !inherits(functions, object, &wanted, &READ) {
            return None;
        }
        // SAFETY: the object's own (inherited) function, on the game thread.
        let count = unsafe { std::mem::transmute::<usize, CountFn>(functions.count)(object) };
        (1..=MOST_ELEMENTS).contains(&count).then_some(Self { object, functions, count })
    }

    /// Element `element`'s matrix against its parent, where the model can be written.
    pub fn local(&self, element: i32) -> Option<Mat34> {
        let (local, _) = self.writable()?;
        self.matrix(local, element)
    }

    /// Sets element `element`'s matrix against its parent (it moves its children with it); false
    /// where the model cannot be written.
    pub fn set_local(&self, element: i32, matrix: &Mat34) -> bool {
        let Some((_, set)) = self.writable().filter(|_| (0..self.count).contains(&element)) else { return false };
        self.set(set, element, matrix);
        true
    }

    /// The local matrix's read and write, when the class keeps them.
    fn writable(&self) -> Option<(usize, usize)> {
        let (local, set) = self.functions.local?;
        inherits(self.functions, self.object, &[(local, "local matrix"), (set, "local matrix's write")], &WRITE).then_some((local, set))
    }

    /// The world matrix's write and the bone test, when the class keeps them.
    fn posable(&self) -> Option<(usize, usize)> {
        let (set_world, is_bone) = self.functions.pose?;
        inherits(self.functions, self.object, &[(set_world, "world matrix's write"), (is_bone, "bone test")], &POSE).then_some((set_world, is_bone))
    }

    fn matrix(&self, function: usize, element: i32) -> Option<Mat34> {
        if !(0..self.count).contains(&element) {
            return None;
        }
        // SAFETY: a valid element index; the engine returns a reference to its matrix.
        let matrix = unsafe { std::mem::transmute::<usize, MatrixFn>(function)(self.object, element) };
        mem::read::<Mat34>(matrix).filter(|m| m.iter().all(|c| c.is_finite()))
    }

    fn set(&self, function: usize, element: i32, matrix: &Mat34) {
        let aligned = monaka_core::Aligned16(*matrix);
        // SAFETY: a valid element index and a 12-float matrix that lives across the call, on the
        // game thread.
        unsafe { std::mem::transmute::<usize, SetMatrixFn>(function)(self.object, element, aligned.0.as_ptr()) };
    }

    /// The first element whose name (without case) is `name` (`ArmsSkeleton::find` matches the
    /// case too).
    pub fn find_any_case(&self, name: &str) -> Option<i32> {
        (0..self.count).find(|&e| self.name(e).eq_ignore_ascii_case(name))
    }

    /// The first element whose name (without case) holds `part`.
    pub fn find_like(&self, part: &str) -> Option<i32> {
        (0..self.count).find(|&e| self.name(e).to_ascii_lowercase().contains(part))
    }

    /// `hand`'s descendants, parents first.
    pub fn under(&self, hand: i32) -> Vec<i32> {
        let mut under = vec![hand];
        let mut k = 0;
        while k < under.len() {
            let parent = under[k];
            under.extend((0..self.count).filter(|&e| self.parent(e) == parent));
            k += 1;
        }
        under
    }

    /// `under`'s local matrices, then `read` with `hand`'s descendants reset to the reference frame
    /// (the rest pose), then the local matrices put back; none where the engine has no reset or the
    /// model cannot be written.
    pub fn at_rest<R>(&self, hand: i32, under: &[i32], read: impl FnOnce(&Self) -> R) -> Option<R> {
        let reset = self.functions.reset?;
        let (_, set_local) = self.writable()?;
        let saved: Vec<(i32, Option<Mat34>)> = under.iter().map(|&e| (e, self.local(e))).collect();
        // SAFETY: the engine's own reset, on the game thread, for an element of this model.
        unsafe { std::mem::transmute::<usize, ResetFn>(reset)(self.object, hand) };
        let result = read(self);
        for (e, matrix) in saved {
            if let Some(matrix) = matrix {
                self.set(set_local, e, &matrix);
            }
        }
        Some(result)
    }
}

impl Skeleton for Model {
    fn id(&self) -> usize {
        self.object
    }

    fn count(&self) -> i32 {
        self.count
    }

    fn name(&self, element: i32) -> String {
        // SAFETY: a valid element index; the engine returns its name or null.
        let text = unsafe { std::mem::transmute::<usize, NameFn>(self.functions.name)(self.object, element) };
        if text == 0 { String::new() } else { mem::read_c_string(text, 64).unwrap_or_default() }
    }

    fn parent(&self, element: i32) -> i32 {
        // SAFETY: a valid element index.
        unsafe { std::mem::transmute::<usize, ParentFn>(self.functions.parent)(self.object, element) }
    }

    /// The engine keeps no rest pose apart: `root`'s descendants are reset to the reference frame,
    /// read and put back.
    fn rests_under(&self, root: i32, elements: &[i32]) -> Vec<Option<Mat34>> {
        let under = self.under(root);
        self.at_rest(root, &under, |model| elements.iter().map(|&e| model.local(e)).collect()).unwrap_or_else(|| vec![None; elements.len()])
    }

    /// Element `element`'s world matrix (rows; the axes may carry a scale).
    fn world(&self, element: i32) -> Option<Mat34> {
        self.matrix(self.functions.world, element)
    }

    /// Sets element `element`'s world matrix, where the model can be posed.
    fn set_world(&self, element: i32, matrix: &Mat34) {
        if let Some((set_world, _)) = self.posable().filter(|_| (0..self.count).contains(&element)) {
            self.set(set_world, element, matrix);
        }
    }

    fn is_bone(&self, element: i32) -> bool {
        let Some((_, is_bone)) = self.posable().filter(|_| (0..self.count).contains(&element)) else { return false };
        // SAFETY: a valid element index; a plain lookup despite the non-const signature.
        unsafe { std::mem::transmute::<usize, IsBoneFn>(is_bone)(self.object, element) }
    }
}

impl ArmsSkeleton for Model {}
