//! A model visual's elements (a held weapon's visual is an `IModelObject` at its start, as the bow's
//! `BowVis` is), read through the engine's element exports. Those are virtuals, so a model is read
//! through them only when its vtable holds each one (its class does not override it), and written
//! only when it also holds the local matrix's two; a vtable in the game DLL reaches them through a
//! jump to the import. A local matrix set stays across the engine's next update of the model (it
//! marks the model for one), which works the world matrices out from the locals.

use monaka_core::camera::Mat34;
use monaka_hook::mem;
use monaka_hook::module::Module;
use monaka_producer::log;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

const COUNT: &str = "?GetElementsNumber@IModelObject@@UEBAHXZ";
const NAME: &str = "?GetElementNameCStr@IModelObject@@UEBAPEBDH@Z";
const PARENT: &str = "?GetElementParent@IModelObject@@UEBAHH@Z";
const WORLD: &str = "?GetElementWorldMatrix@IModelObject@@UEBAAEBVmtx34@@H@Z";
const LOCAL: &str = "?GetElementLocalMatrix@IModelObject@@UEBAAEBVmtx34@@H@Z";
const SET_LOCAL: &str = "?SetElementLocalMatrix@IModelObject@@UEAAXHAEBVmtx34@@@Z";
/// More elements than this is not a weapon's model.
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

struct Functions {
    count: usize,
    name: usize,
    parent: usize,
    world: usize,
    /// The local matrix's read and write, where exported.
    local: Option<(usize, usize)>,
}

static FUNCTIONS: OnceLock<Option<Functions>> = OnceLock::new();

/// The vtable last seen to hold a set of functions, and the last one seen not to.
struct Verdicts {
    holds: AtomicU64,
    lacks: AtomicU64,
}

static READ: Verdicts = Verdicts { holds: AtomicU64::new(0), lacks: AtomicU64::new(0) };
static WRITE: Verdicts = Verdicts { holds: AtomicU64::new(0), lacks: AtomicU64::new(0) };

fn functions() -> Option<&'static Functions> {
    FUNCTIONS
        .get_or_init(|| {
            let engine = Module::find(crate::ENGINE)?;
            let found = (|| {
                Some(Functions {
                    count: engine.export(COUNT)?,
                    name: engine.export(NAME)?,
                    parent: engine.export(PARENT)?,
                    world: engine.export(WORLD)?,
                    local: engine.export(LOCAL).zip(engine.export(SET_LOCAL)),
                })
            })();
            if found.is_none() {
                log!("model elements: the engine does not export them");
            }
            found
        })
        .as_ref()
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

/// Whether `object`'s class keeps each of the engine's `wanted` functions (checked once per vtable,
/// the verdict kept in `verdicts`).
fn inherits(object: usize, wanted: &[(usize, &str)], verdicts: &Verdicts) -> bool {
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

/// A model's elements.
pub struct Model {
    object: usize,
    functions: &'static Functions,
    count: i32,
}

impl Model {
    /// The model at the start of the visual `object`, when its class keeps the engine's element
    /// functions.
    pub fn of(object: usize) -> Option<Self> {
        let functions = functions()?;
        let wanted = [(functions.count, "count"), (functions.name, "name"), (functions.parent, "parent"), (functions.world, "world matrix")];
        if object == 0 || !inherits(object, &wanted, &READ) {
            return None;
        }
        // SAFETY: the object's own (inherited) virtual, on the game thread.
        let count = unsafe { std::mem::transmute::<usize, CountFn>(functions.count)(object) };
        (1..=MOST_ELEMENTS).contains(&count).then_some(Self { object, functions, count })
    }

    pub fn id(&self) -> usize {
        self.object
    }

    pub fn count(&self) -> i32 {
        self.count
    }

    pub fn name(&self, element: i32) -> String {
        // SAFETY: a valid element index; the engine returns its name or null.
        let text = unsafe { std::mem::transmute::<usize, NameFn>(self.functions.name)(self.object, element) };
        if text == 0 { String::new() } else { mem::read_c_string(text, 64).unwrap_or_default() }
    }

    pub fn parent(&self, element: i32) -> i32 {
        // SAFETY: a valid element index.
        unsafe { std::mem::transmute::<usize, ParentFn>(self.functions.parent)(self.object, element) }
    }

    /// Element `element`'s world matrix (rows; the axes may carry a scale).
    pub fn world(&self, element: i32) -> Option<Mat34> {
        self.matrix(self.functions.world, element)
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
        let aligned = monaka_core::Aligned16(*matrix);
        // SAFETY: a valid element index and a 12-float matrix that lives across the call, on the
        // game thread.
        unsafe { std::mem::transmute::<usize, SetMatrixFn>(set)(self.object, element, aligned.0.as_ptr()) };
        true
    }

    /// The local matrix's read and write, when the class keeps them.
    fn writable(&self) -> Option<(usize, usize)> {
        let (local, set) = self.functions.local?;
        inherits(self.object, &[(local, "local matrix"), (set, "local matrix's write")], &WRITE).then_some((local, set))
    }

    fn matrix(&self, function: usize, element: i32) -> Option<Mat34> {
        if !(0..self.count).contains(&element) {
            return None;
        }
        // SAFETY: a valid element index; the engine returns a reference to its matrix.
        let matrix = unsafe { std::mem::transmute::<usize, MatrixFn>(function)(self.object, element) };
        mem::read::<Mat34>(matrix).filter(|m| m.iter().all(|c| c.is_finite()))
    }

    /// The first element whose name (without case) is `name`.
    pub fn find(&self, name: &str) -> Option<i32> {
        (0..self.count).find(|&e| self.name(e).eq_ignore_ascii_case(name))
    }

    /// The first element whose name (without case) holds `part`.
    pub fn find_like(&self, part: &str) -> Option<i32> {
        (0..self.count).find(|&e| self.name(e).to_ascii_lowercase().contains(part))
    }
}
