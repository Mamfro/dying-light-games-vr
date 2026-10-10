//! The HUD as the engine knows it: Dying Light 2's UI is the engine's `gui::` tree of documents
//! (a `gui::IGroup` root with named `gui::IElement`s under it), reached through the engine's own
//! exported functions. A document is found from the elements the game hands two of those
//! functions (`IsActuallyVisible`, `GetActualOpacity`, hooked), followed up with `GetDocumentRoot`.
//! Each element wraps a `gui::IObject` at offset 0 (its RTTI bases), whose `GetName`
//! is the element's name.
//!
//! The dynamic HUD ([`crate::panels`]) takes its pieces' boxes from here: each piece is a widget
//! of a document (the game's [`Piece`] table), found by name from the root once and read every frame
//! ([`layout`]: the union of its shown leaves' boxes on screen, from their world matrices).
//!
//! The probe (`probe-gui.enable`) logs each document met once with its widgets (the root's
//! children: name, class, visibility, opacity, place and size), the shown tree under each, and
//! which widgets are visible whenever that changes.
//!
//! The exported `gui::IElement` getters are thin handles: `mov rax, [rcx+0x40]` to the element's
//! implementation (`gui::CElement`) and a field read, or a jump on to it. The game never calls
//! the exported ones, so three of the
//! implementations they jump to are hooked instead (`GetWorldMatrix`'s, `GetActualPos`'s and
//! `GetActualSize`'s, which the engine's own layout calls; each build's addresses and first bytes
//! in its [`Build`]); those get the implementation object, which a stand-in handle (its +0x40 the
//! implementation) hands to the exported `GetDocumentRoot`, and from that root on everything is
//! real interfaces. Dying Light 2 and The Beast share this; each supplies its [`Build`] and pieces.

use monaka_hook::module::Module;
use monaka_hook::probe::class_name;
use monaka_hook::{Hooks, InFlight, Instruction, Original, mem};
use std::sync::atomic::AtomicUsize;
use monaka_producer::{Rejection, log};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

const IS_ACTUALLY_VISIBLE: &str = "?IsActuallyVisible@IElement@gui@@QEBA_NXZ";
const GET_VISIBLE: &str = "?GetVisible@IElement@gui@@QEBA_NXZ";
const GET_ACTUAL_OPACITY: &str = "?GetActualOpacity@IElement@gui@@QEBAMXZ";
const GET_OPACITY: &str = "?GetOpacity@IElement@gui@@QEBAMXZ";
const GET_DOCUMENT_ROOT: &str = "?GetDocumentRoot@IElement@gui@@QEBAPEAVIGroup@2@XZ";
/// An element's display list (`gui::IObject` at offset 0), and the list's canvas: an `ICanva` is
/// an `IGroup` (its RTTI bases, offset 0) whose children are the list's documents.
const GET_DISPLAY_LIST: &str = "?GetIDisplayList@IObject@gui@@QEBAPEAVIDisplayList@2@XZ";
const GET_CANVA: &str = "?GetCanva@IDisplayList@gui@@QEBAPEAVICanva@2@XZ";
const GET_ACTUAL_POS: &str = "?GetActualPos@IElement@gui@@QEAAAEBVvec3@@XZ";
const GET_ACTUAL_SIZE: &str = "?GetActualSize@IElement@gui@@QEAAAEBVvec3@@XZ";
const GET_WORLD_MATRIX: &str = "?GetWorldMatrix@IElement@gui@@QEAAAEBVmtx34@@XZ";
const GET_CHILDREN_COUNT: &str = "?GetChildrenCount@IGroup@gui@@QEBAHXZ";
const GET_CHILD: &str = "?GetChild@IGroup@gui@@QEBAPEAVIElement@2@H@Z";
const CAST_TO_GROUP: &str = "?CastToIGroup@IGroup@gui@@SAPEAV12@PEAVIElement@2@@Z";
const GET_NAME: &str = "?GetName@IObject@gui@@QEBAPEBDXZ";

type ThisFn = unsafe extern "system" fn(*mut c_void) -> usize;
type BoolFn = unsafe extern "system" fn(*mut c_void) -> bool;
type FloatFn = unsafe extern "system" fn(*mut c_void) -> f32;
type RefFn = unsafe extern "system" fn(*mut c_void) -> *const f32;
type CountFn = unsafe extern "system" fn(*mut c_void) -> i32;
type IndexFn = unsafe extern "system" fn(*mut c_void, i32) -> usize;
type CastFn = unsafe extern "system" fn(usize) -> usize;
type NameFn = unsafe extern "system" fn(*mut c_void) -> *const u8;

static WORLD_IMPL_ORIGINAL: Original<RefFn> = Original::new();
static POSITION_IMPL_ORIGINAL: Original<RefFn> = Original::new();
static SIZE_IMPL_ORIGINAL: Original<RefFn> = Original::new();

/// One build's implementations of the exported getters (`gui::CElement` methods: the RVA each
/// export jumps to, with its first instructions, checked before the hook goes in).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    pub world: (usize, &'static [Instruction]),
    pub position: (usize, &'static [Instruction]),
    pub size: (usize, &'static [Instruction]),
}

/// The exported `GetWorldMatrix`, `GetActualPos` and `GetActualSize` must jump to the build's
/// implementations: `mov rcx, [rcx+0x40]; jmp rel32`.
const WRAPPER_JUMP: [u8; 5] = [0x48, 0x8b, 0x49, 0x40, 0xe9];

/// Calls of each hooked implementation: world matrix, actual position, actual size.
static CALLED: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// Checks that the exported wrapper at `export` jumps to `implementation`.
fn jumps_to(export: usize, implementation: usize) -> bool {
    let code: Option<[u8; 9]> = mem::read(export);
    code.is_some_and(|c| c[..5] == WRAPPER_JUMP && (export as i64 + 9 + i32::from_le_bytes([c[5], c[6], c[7], c[8]]) as i64) == implementation as i64)
}

/// The engine's gui getters.
struct Calls {
    visible: BoolFn,
    actually_visible: BoolFn,
    opacity: FloatFn,
    actual_opacity: FloatFn,
    root: ThisFn,
    display_list: ThisFn,
    canva: ThisFn,
    position: RefFn,
    size: RefFn,
    world: RefFn,
    children: CountFn,
    child: IndexFn,
    group: CastFn,
    name: NameFn,
}

static CALLS: Mutex<Option<Calls>> = Mutex::new(None);
/// Set while a snapshot runs: the engine calls it makes come through the same hooks.
static BUSY: AtomicBool = AtomicBool::new(false);
static SNAPSHOT_AT: AtomicU64 = AtomicU64::new(0);
/// Snapshots are taken at most this often (ms) while probing.
const SNAPSHOT_EVERY_MS: u64 = 2000;
/// Whether the probe logs, and whether the pieces are read for the dynamic HUD.
static PROBING: AtomicBool = AtomicBool::new(false);
static PIECES_WANTED: AtomicBool = AtomicBool::new(false);

/// How a piece's box is read: the widget's own box as laid out (stable; for widgets whose parts
/// float about in it, like the compass's markers), or the union of its shown leaves (for widgets
/// whose box is mostly empty, like health's).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Extent {
    Declared,
    Leaves,
}

/// One piece of the HUD the dynamic HUD takes: the panel it goes on (the viewer's pieces, as
/// Dying Light 1 fills them: `minimap`, `quests`, `weapon`, `health`, `tool`), the document, the
/// path of names from its root (the first child of each name at each step), how its box is read,
/// and its panel's size in pixels (the viewer gives each a fixed width on the hand; the height
/// follows the shape).
#[derive(Clone, Copy, Debug)]
pub struct Piece {
    pub name: &'static str,
    pub document: &'static str,
    pub path: &'static [&'static str],
    pub extent: Extent,
    pub size: (u32, u32),
}

/// The most pieces a game may take.
pub const MAX_PIECES: usize = 8;

/// The pieces' boxes on screen this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Layout {
    /// Per piece: left, top, right, bottom in screen pixels, when shown.
    pub pieces: [Option<[f32; 4]>; MAX_PIECES],
    /// The root's size on screen (its layout's; the boxes are in the frame's pixels, where the
    /// root may sit offset: letterboxed in a square eye).
    pub screen: [f32; 2],
    /// The root's place in the frame (its top left).
    pub origin: [f32; 2],
    /// When it was read (`monaka_channel::tick`).
    pub taken: u64,
}

static LAYOUT: Mutex<Option<Layout>> = Mutex::new(None);
/// Each piece's element, once found: (its document's root, the element).
static FOUND: Mutex<[Option<(usize, usize)>; MAX_PIECES]> = Mutex::new([None; MAX_PIECES]);
/// The game's pieces, and whether its HUD is live (the frames count) now.
static PIECES: Mutex<&'static [Piece]> = Mutex::new(&[]);
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// The game's pieces.
pub fn pieces() -> &'static [Piece] {
    PIECES.lock().map(|p| *p).unwrap_or(&[])
}

fn active() -> bool {
    let f = ACTIVE.load(Relaxed);
    // SAFETY: set from an `fn() -> bool` by `configure` (0: none).
    f != 0 && unsafe { std::mem::transmute::<usize, fn() -> bool>(f)() }
}
/// Document roots by name, as met.
static ROOTS: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());
static LAYOUT_AT: AtomicU64 = AtomicU64::new(0);
/// The pieces are read at most this often (ms): once a frame.
const LAYOUT_EVERY_MS: u64 = 8;

/// The latest reading of the pieces.
pub fn layout() -> Option<Layout> {
    LAYOUT.lock().ok().and_then(|l| *l)
}

/// What this run wants of the tree: the probe's log, and the game's `pieces` for the dynamic HUD
/// (none: not read); `active` says whether the game's HUD is live now (VR running, in play).
pub fn configure(probe: bool, pieces: &'static [Piece], active: fn() -> bool) {
    PROBING.store(probe, Relaxed);
    *PIECES.lock().unwrap_or_else(|e| e.into_inner()) = &pieces[..pieces.len().min(MAX_PIECES)];
    PIECES_WANTED.store(!pieces.is_empty(), Relaxed);
    ACTIVE.store(active as usize, Relaxed);
}

/// Looks the engine's gui functions up and hooks `build`'s three implementations.
pub fn install(hooks: &mut Hooks, engine: &Module, build: &Build) -> Result<(), Rejection> {
    let find = |name: &str| engine.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    // SAFETY: each export has the signature its mangled name states, on x64 (references come
    // back as pointers, `CastToIGroup` is static).
    let calls = unsafe {
        Calls {
            visible: std::mem::transmute::<usize, BoolFn>(find(GET_VISIBLE)?),
            actually_visible: std::mem::transmute::<usize, BoolFn>(find(IS_ACTUALLY_VISIBLE)?),
            opacity: std::mem::transmute::<usize, FloatFn>(find(GET_OPACITY)?),
            actual_opacity: std::mem::transmute::<usize, FloatFn>(find(GET_ACTUAL_OPACITY)?),
            root: std::mem::transmute::<usize, ThisFn>(find(GET_DOCUMENT_ROOT)?),
            display_list: std::mem::transmute::<usize, ThisFn>(find(GET_DISPLAY_LIST)?),
            canva: std::mem::transmute::<usize, ThisFn>(find(GET_CANVA)?),
            position: std::mem::transmute::<usize, RefFn>(find(GET_ACTUAL_POS)?),
            size: std::mem::transmute::<usize, RefFn>(find(GET_ACTUAL_SIZE)?),
            world: std::mem::transmute::<usize, RefFn>(find(GET_WORLD_MATRIX)?),
            children: std::mem::transmute::<usize, CountFn>(find(GET_CHILDREN_COUNT)?),
            child: std::mem::transmute::<usize, IndexFn>(find(GET_CHILD)?),
            group: std::mem::transmute::<usize, CastFn>(find(CAST_TO_GROUP)?),
            name: std::mem::transmute::<usize, NameFn>(find(GET_NAME)?),
        }
    };
    let (world, position, size) = (engine.at(build.world.0), engine.at(build.position.0), engine.at(build.size.0));
    if !jumps_to(find(GET_WORLD_MATRIX)?, world) || !jumps_to(find(GET_ACTUAL_POS)?, position) || !jumps_to(find(GET_ACTUAL_SIZE)?, size) {
        return Err(Rejection::revision("the gui getters do not jump to the implementations measured"));
    }
    // SAFETY: the detours have the implementations' signatures (the element's implementation in,
    // a reference out); each target's first instructions are checked against this build's.
    unsafe {
        hooks.inline(&WORLD_IMPL_ORIGINAL, "gui::CElement world matrix", world, build.world.1, world_matrix as RefFn)?;
        hooks.inline(&POSITION_IMPL_ORIGINAL, "gui::CElement actual position", position, build.position.1, actual_position as RefFn)?;
        hooks.inline(&SIZE_IMPL_ORIGINAL, "gui::CElement actual size", size, build.size.1, actual_size as RefFn)?;
    }
    *CALLS.lock().unwrap_or_else(|e| e.into_inner()) = Some(calls);
    Ok(())
}

unsafe extern "system" fn world_matrix(implementation: *mut c_void) -> *const f32 {
    let _flight = InFlight::enter();
    CALLED[0].fetch_add(1, Relaxed);
    seen(implementation as usize);
    // SAFETY: forwards the engine's own call.
    unsafe { WORLD_IMPL_ORIGINAL.get()(implementation) }
}

unsafe extern "system" fn actual_position(implementation: *mut c_void) -> *const f32 {
    let _flight = InFlight::enter();
    CALLED[1].fetch_add(1, Relaxed);
    seen(implementation as usize);
    // SAFETY: forwards the engine's own call.
    unsafe { POSITION_IMPL_ORIGINAL.get()(implementation) }
}

unsafe extern "system" fn actual_size(implementation: *mut c_void) -> *const f32 {
    let _flight = InFlight::enter();
    CALLED[2].fetch_add(1, Relaxed);
    seen(implementation as usize);
    // SAFETY: forwards the engine's own call.
    unsafe { SIZE_IMPL_ORIGINAL.get()(implementation) }
}

/// The hooked implementations' call counts, for the log.
fn called() -> String {
    format!("world matrix {}, actual position {}, actual size {}", CALLED[0].load(Relaxed), CALLED[1].load(Relaxed), CALLED[2].load(Relaxed))
}

/// A stand-in `gui::IElement` handle for an implementation object: the exported getters read
/// only its +0x40 (the implementation), so one made here serves to ask for the document root.
/// Never handed to anything virtual (`CastToIGroup` calls through the handle's vtable).
#[repr(C)]
struct Handle([usize; 9]);

impl Handle {
    fn of(implementation: usize) -> Self {
        let mut words = [0usize; 9];
        words[8] = implementation;
        Self(words)
    }
}

/// An element's implementation the engine's layout is working on: a snapshot of its document
/// when one is due.
fn seen(implementation: usize) {
    if implementation == 0 || BUSY.load(Relaxed) || !active() {
        return;
    }
    let Ok(calls) = CALLS.lock() else { return };
    let Some(calls) = calls.as_ref() else { return };
    BUSY.store(true, Relaxed);
    let mut handle = Handle::of(implementation);
    // SAFETY: the exported getter reads the handle's +0x40 (this implementation, which the
    // engine itself is working on) and follows the engine's own document pointer (game thread).
    let root = unsafe { (calls.root)(handle.0.as_mut_ptr().cast()) };
    let now = monaka_channel::tick();
    if root != 0 {
        note_root(calls, root);
        // The probe: a document not met before is read at once, the others again every so often.
        let known = DOCUMENTS.lock().is_ok_and(|d| d.contains_key(&root));
        if PROBING.load(Relaxed) && (!known || now.saturating_sub(SNAPSHOT_AT.load(Relaxed)) >= SNAPSHOT_EVERY_MS) {
            if known {
                SNAPSHOT_AT.store(now, Relaxed);
            }
            let started = std::time::Instant::now();
            snapshot(calls, root, implementation);
            note_cost(started.elapsed());
        }
    }
    if PIECES_WANTED.load(Relaxed) && now.saturating_sub(LAYOUT_AT.load(Relaxed)) >= LAYOUT_EVERY_MS {
        LAYOUT_AT.store(now, Relaxed);
        let started = std::time::Instant::now();
        read_pieces(calls, now);
        note_layout_cost(started.elapsed());
    }
    BUSY.store(false, Relaxed);
}

/// Remembers a document root by its name (the names the pieces are addressed by), and every
/// other document of its display list (under the list's canvas). Dying Light 2 puts each HUD
/// layer on a list of its own, so a layer is met when the engine lays something of
/// it out: the main layer (stamina, health, objectives) within seconds of play, later when
/// standing still.
fn note_root(calls: &Calls, root: usize) {
    let Ok(mut roots) = ROOTS.lock() else { return };
    if roots.iter().any(|(_, r)| *r == root) {
        return;
    }
    let mut remember = |root: usize| {
        let name = name_of(calls, root);
        // A document reloaded (a level change) gets a new root: the old one is dropped.
        roots.retain(|(n, _)| *n != name);
        roots.push((name, root));
    };
    remember(root);
    // SAFETY: the engine's own getters on live elements of the tree (game thread): the root's
    // display list (`IObject` at offset 0) and its canvas (an `IGroup`).
    let list = unsafe { (calls.display_list)(root as *mut c_void) };
    let canva = if list != 0 { unsafe { (calls.canva)(list as *mut c_void) } } else { 0 };
    if canva == 0 {
        return;
    }
    let mut others = Vec::new();
    let mut pending = vec![(canva, 0usize)];
    while let Some((element, depth)) = pending.pop() {
        // SAFETY: as above.
        let count = unsafe {
            let group = (calls.group)(element);
            if group == 0 {
                continue;
            }
            (calls.children)(group as *mut c_void)
        };
        for child in children(calls, element, count) {
            // A child that is its own document root is a document; a group is looked into.
            if unsafe { (calls.root)(child as *mut c_void) } == child {
                if child != root {
                    others.push(name_of(calls, child));
                    remember(child);
                }
            } else if depth < 2 {
                pending.push((child, depth + 1));
            }
        }
    }
    if PROBING.load(Relaxed) {
        log!("gui: on the display list of '{}': {}", name_of(calls, root), if others.is_empty() { "no other document".to_owned() } else { others.join(", ") });
    }
}

/// Element `name` among the children of `element` (the first of that name).
fn child_named(calls: &Calls, element: usize, name: &str) -> Option<usize> {
    // SAFETY: the engine's own cast and getters on a live element (game thread).
    let count = unsafe {
        let group = (calls.group)(element);
        if group == 0 {
            return None;
        }
        (calls.children)(group as *mut c_void)
    };
    children(calls, element, count).into_iter().find(|&c| name_of(calls, c) == name)
}

/// The box (left, top, right, bottom; screen pixels) of the shown leaves under `element`, `depth`
/// levels down at most, into `found`. Only the getters the box needs (no names or classes: this
/// runs every frame).
fn leaf_box(calls: &Calls, element: usize, depth: usize, found: &mut Option<[f32; 4]>, visited: &mut usize) {
    *visited += 1;
    if depth > 8 || *visited > 256 {
        return;
    }
    let e = element as *mut c_void;
    // SAFETY: the engine's own getters on a live element of the tree (game thread); the
    // references point at the element's own vec3 and mtx34.
    let (shown, count) = unsafe {
        if !(calls.actually_visible)(e) || (calls.actual_opacity)(e) <= 0.0 {
            return;
        }
        let group = (calls.group)(element);
        let count = if group != 0 { (calls.children)(group as *mut c_void) } else { 0 };
        (count > 0, count)
    };
    if shown {
        for child in children(calls, element, count) {
            leaf_box(calls, child, depth + 1, found, visited);
        }
        return;
    }
    // SAFETY: as above.
    let (world, size) = unsafe { (mem::read::<[f32; 12]>((calls.world)(e) as usize), mem::read::<[f32; 3]>((calls.size)(e) as usize)) };
    if let (Some(m), Some(size)) = (world, size) {
        let scale = m[0].abs();
        if m[3].is_finite() && m[7].is_finite() && size[0].is_finite() && size[1].is_finite() && scale.is_finite() {
            let b = [m[3], m[7], m[3] + size[0] * scale, m[7] + size[1] * scale];
            *found = Some(found.map_or(b, |f| monaka_core::hud::union(f, b)));
        }
    }
}

/// Reads the pieces' boxes this frame: each piece's element found by its path once (and again
/// when its document's root changes or its name no longer matches), then its shown leaves' box.
fn read_pieces(calls: &Calls, now: u64) {
    let Ok(roots) = ROOTS.lock() else { return };
    let Ok(mut found) = FOUND.lock() else { return };
    let mut layout = Layout { pieces: [None; MAX_PIECES], screen: [0.0; 2], origin: [0.0; 2], taken: now };
    for (i, piece) in pieces().iter().enumerate() {
        let (document, path, extent) = (piece.document, piece.path, &piece.extent);
        let Some(&(_, root)) = roots.iter().find(|(name, _)| name == document) else { continue };
        if layout.screen[0] <= 0.0 {
            let top = read(calls, root);
            let scale = top.scale.abs();
            if top.size[0].is_finite() && scale.is_finite() && scale > 0.0 {
                layout.screen = [top.size[0] * scale, top.size[1] * scale];
                layout.origin = [top.world[0], top.world[1]];
            }
        }
        let element = match found[i] {
            Some((r, element)) if r == root && name_of(calls, element) == *path.last().unwrap_or(&"") => Some(element),
            _ => {
                let element = path.iter().try_fold(root, |at, name| child_named(calls, at, name));
                found[i] = element.map(|e| (root, e));
                element
            }
        };
        let Some(element) = element else { continue };
        // The widget's own box as laid out (its place and size on screen), or its shown leaves'.
        // Markers that float beyond the box (the compass's location icons) are left to the eyes.
        let node = read(calls, element);
        if !node.actually_visible || node.actual_opacity <= 0.0 {
            continue;
        }
        let scale = node.scale.abs();
        let own = (*extent == Extent::Declared && node.size[0] > 1.0 && node.size[1] > 1.0 && scale.is_finite() && scale > 0.0 && node.world[0].is_finite())
            .then(|| [node.world[0], node.world[1], node.world[0] + node.size[0] * scale, node.world[1] + node.size[1] * scale]);
        let b = own.or_else(|| {
            let mut b = None;
            let mut visited = 0;
            leaf_box(calls, element, 0, &mut b, &mut visited);
            b
        });
        layout.pieces[i] = b.filter(|b| b[2] > b[0] && b[3] > b[1]);
    }
    note_pieces(&layout);
    if let Ok(mut latest) = LAYOUT.lock() {
        *latest = Some(layout);
    }
}

/// Logs which pieces are shown whenever that changes, and the first boxes.
fn note_pieces(layout: &Layout) {
    static SHOWN: AtomicU64 = AtomicU64::new(u64::MAX);
    let shown = layout.pieces.iter().enumerate().filter(|(_, b)| b.is_some()).fold(0u64, |bits, (i, _)| bits | 1 << i);
    if SHOWN.swap(shown, Relaxed) != shown {
        let names: Vec<String> = pieces().iter().zip(layout.pieces).filter_map(|(piece, b)| b.map(|b| format!("{} [{:.0} {:.0} {:.0} {:.0}]", piece.name, b[0], b[1], b[2], b[3]))).collect();
        log!(
            "dynamic HUD: pieces shown: {} (layout {:.0}x{:.0} at [{:.0} {:.0}] in the frame)",
            if names.is_empty() { "none".to_owned() } else { names.join(", ") },
            layout.screen[0],
            layout.screen[1],
            layout.origin[0],
            layout.origin[1]
        );
    }
}

/// What reading the pieces costs the game thread: logged after 100 and every 10000 readings.
fn note_layout_cost(took: std::time::Duration) {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    static TOTAL_US: AtomicU64 = AtomicU64::new(0);
    let us = took.as_micros() as u64;
    let n = COUNT.fetch_add(1, Relaxed) + 1;
    let total = TOTAL_US.fetch_add(us, Relaxed) + us;
    if n == 100 || n.is_multiple_of(10000) {
        log!("dynamic HUD: {n} readings of the pieces, {} us each on average", total / n);
    }
}

/// An element's name through `gui::IObject::GetName` (the `IObject` sits at offset 0).
fn name_of(calls: &Calls, element: usize) -> String {
    // SAFETY: the engine's own getter on a live element (game thread).
    let text = unsafe { (calls.name)(element as *mut c_void) };
    mem::read_c_string(text as usize, 96).unwrap_or_default()
}

/// One element as the probe reads it.
struct Node {
    element: usize,
    name: String,
    class: String,
    visible: bool,
    actually_visible: bool,
    opacity: f32,
    actual_opacity: f32,
    at: [f32; 3],
    size: [f32; 3],
    world: [f32; 3],
    /// The world matrix's x scale: layout pixels to screen pixels.
    scale: f32,
    children: i32,
}

fn read(calls: &Calls, element: usize) -> Node {
    let e = element as *mut c_void;
    // SAFETY: the engine's own getters on a live element of the tree (game thread); the
    // references point at the element's own vec3 and mtx34.
    unsafe {
        let group = (calls.group)(element);
        let children = if group != 0 { (calls.children)(group as *mut c_void) } else { 0 };
        let world = mem::read::<[f32; 12]>((calls.world)(e) as usize);
        Node {
            element,
            name: name_of(calls, element),
            class: class_name(element).unwrap_or_default(),
            visible: (calls.visible)(e),
            actually_visible: (calls.actually_visible)(e),
            opacity: (calls.opacity)(e),
            actual_opacity: (calls.actual_opacity)(e),
            at: mem::read::<[f32; 3]>((calls.position)(e) as usize).unwrap_or([f32::NAN; 3]),
            size: mem::read::<[f32; 3]>((calls.size)(e) as usize).unwrap_or([f32::NAN; 3]),
            world: world.map_or([f32::NAN; 3], |m| [m[3], m[7], m[11]]),
            scale: world.map_or(f32::NAN, |m| m[0]),
            children,
        }
    }
}

fn children(calls: &Calls, element: usize, count: i32) -> Vec<usize> {
    // SAFETY: the engine's own cast and getter, indices under the group's count (game thread).
    unsafe {
        let group = (calls.group)(element);
        if group == 0 {
            return Vec::new();
        }
        (0..count.clamp(0, 1024)).map(|i| (calls.child)(group as *mut c_void, i)).filter(|&c| c != 0).collect()
    }
}

impl Node {
    fn line(&self) -> String {
        format!(
            "{:#x} '{}' {} vis {}/{} opacity {:.2}/{:.2} at [{:.1} {:.1} {:.1}] size [{:.1} {:.1}] world [{:.1} {:.1} {:.1}] children {}",
            self.element,
            self.name,
            self.class,
            self.visible,
            self.actually_visible,
            self.opacity,
            self.actual_opacity,
            self.at[0],
            self.at[1],
            self.at[2],
            self.size[0],
            self.size[1],
            self.world[0],
            self.world[1],
            self.world[2],
            self.children
        )
    }
}

/// Documents logged in full already (root address: its name), and each one's visible widgets as
/// last logged.
static DOCUMENTS: Mutex<BTreeMap<usize, (String, Vec<String>)>> = Mutex::new(BTreeMap::new());

fn snapshot(calls: &Calls, root: usize, from: usize) {
    let top = read(calls, root);
    let Ok(mut documents) = DOCUMENTS.lock() else { return };
    let widgets: Vec<Node> = children(calls, root, top.children).into_iter().map(|c| read(calls, c)).collect();
    let visible: Vec<String> = widgets.iter().filter(|w| w.actually_visible && w.actual_opacity > 0.0).map(|w| w.name.clone()).collect();
    match documents.get_mut(&root) {
        Some((_, shown)) if *shown != visible => {
            log!("gui document '{}': visible widgets now: {}", top.name, if visible.is_empty() { "none".to_owned() } else { visible.join(", ") });
            *shown = visible;
        }
        Some(_) => {}
        None => {
            log!("gui document '{}' (from the implementation {from:#x} {}): root {}", top.name, class_name(from).unwrap_or_default(), top.line());
            for widget in &widgets {
                log!("  {}", widget.line());
                if widget.actually_visible && widget.actual_opacity > 0.0 {
                    let mut lines = Vec::new();
                    tree(calls, widget.element, widget.children, 2, &mut lines);
                    for line in lines {
                        log!("{line}");
                    }
                }
            }
            log!("gui document '{}': visible widgets: {}", top.name, if visible.is_empty() { "none".to_owned() } else { visible.join(", ") });
            documents.insert(root, (top.name, visible));
        }
    }
}

/// The shown elements under `element` (visible, with opacity), `depth` levels down at most, as
/// indented lines: name, class, box in the layout (pixels) and on screen (the world matrix's place
/// and scale), for finding the pieces the dynamic HUD takes.
fn tree(calls: &Calls, element: usize, count: i32, depth: usize, lines: &mut Vec<String>) {
    if depth > 8 || lines.len() > 400 {
        return;
    }
    for child in children(calls, element, count) {
        let node = read(calls, child);
        if !node.actually_visible || node.actual_opacity <= 0.0 {
            continue;
        }
        lines.push(format!(
            "{:indent$}'{}' {} layout [{:.0} {:.0}] {:.0}x{:.0} screen [{:.0} {:.0}] scale {:.2}{}",
            "",
            node.name,
            node.class.trim_start_matches(".?AV").trim_end_matches("@@"),
            node.at[0],
            node.at[1],
            node.size[0],
            node.size[1],
            node.world[0],
            node.world[1],
            node.scale,
            if node.children > 0 { format!(" ({} children)", node.children) } else { String::new() },
            indent = depth * 2
        ));
        if node.children > 0 {
            tree(calls, child, node.children, depth + 1, lines);
        }
    }
}

/// What snapshots cost the game thread: logged after 1, 10 and every 100 of them.
fn note_cost(took: std::time::Duration) {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    static TOTAL_US: AtomicU64 = AtomicU64::new(0);
    let us = took.as_micros() as u64;
    let n = COUNT.fetch_add(1, Relaxed) + 1;
    let total = TOTAL_US.fetch_add(us, Relaxed) + us;
    if n == 1 || n == 10 || n.is_multiple_of(100) {
        log!("gui probe: {n} snapshots, {} us each on average; getter calls: {}", total / n, called());
    }
}

/// The end of a run.
pub fn report() {
    if CALLS.lock().is_ok_and(|c| c.is_some()) {
        let documents = ROOTS.lock().map(|r| r.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>()).unwrap_or_default();
        log!("gui: getter calls: {}; documents met: {}", called(), if documents.is_empty() { "none".to_owned() } else { documents.join(", ") });
    }
}
