//! The HUD as the engine knows it, for [`crate::hud::panels`] to tell which widget each HUD draw is.
//!
//! Dying Light 1's HUD is the engine's UI tree under the screen `HUD_DI`: named widgets
//! (`HudRadar`, `health_wrap`, `StackObjectives`, ...) whose leaves are images and texts. The
//! renderer replays the frame's recorded UI commands, so a draw itself carries no element; but
//! the draws follow the tree's drawable leaves, and each one says where it is (a research probe,
//! 2026-10-07): an image draw's matrix puts its quad's corner exactly on its leaf's global
//! position; a text draw's first glyph sits on its text leaf's row (its glyphs are laid out in
//! layout pixels). So once a frame, on the game thread, the drawable leaves are read here, each
//! with its widget, place and kind ([`Layout`]), through the engine's own exported `IUIElement`
//! functions; the panels match each draw against them.
//!
//! The tree is found from the elements the game itself hands three exported UI functions
//! (`GetGlobalTransform`, `IsActuallyVisible`, `IUIText::SetText`), followed up to the root;
//! the snapshot is taken in those hooks, at most every [`SNAPSHOT_EVERY_MS`], from what is
//! cached of the tree ([`Cache`]).
//!
//! Each `IUIElement` wraps an engine object at +0x18 (its name: the string `[[object+8]]`; a
//! text's string at +0x3d8, first its pointer: null when empty). Positions are layout pixels of
//! the HUD's viewport (2644 on a 2644x2644 eye; the HUD's 16:9 lies at y 578).

use monaka_hook::module::Module;
use monaka_hook::probe::class_name;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

/// The widgets each panel shows, in the panels' order (minimap, weapon, quests, health, tool).
pub const WIDGETS: [&str; 5] = ["HudRadar", "HudPrimaryWeaponIndicator", "StackObjectives", "health_wrap", "HudSecondaryWeaponIndicator"];
const ROOT_NAME: &str = "HUD_DI";

/// One drawable leaf: the panel its widget is on (`None`: none, the HUD in the eye keeps it), a
/// text or not, its corner (the global transform's place) and its box (left, top, right, bottom),
/// in layout pixels; and whether it is its widget's last (a draw after it is another widget's).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Leaf {
    pub piece: Option<usize>,
    pub text: bool,
    pub at: [f32; 2],
    pub rect: [f32; 4],
    pub last: bool,
}

/// The HUD's drawable leaves in tree order (the order they are drawn), and each panel widget's
/// box (the union of its leaves; `None`: nothing of it is drawn).
#[derive(Debug, Default)]
pub struct Layout {
    pub leaves: Vec<Leaf>,
    pub pieces: [Option<[f32; 4]>; WIDGETS.len()],
    /// The root's own box (left, top, right, bottom, layout pixels): the space the boxes are in,
    /// which the HUD's draws map onto their viewport. Not the back buffer's size: with FSR the
    /// game renders 1760x1760 and lays the HUD out at the screen's 3840x2160 (2026-10-08).
    pub root: Option<[f32; 4]>,
    /// When it was taken (`monaka_channel::tick`).
    pub taken: u64,
}

/// The latest snapshot.
pub fn layout() -> Option<Arc<Layout>> {
    LAYOUT.lock().ok()?.clone()
}

static LAYOUT: Mutex<Option<Arc<Layout>>> = Mutex::new(None);
/// Snapshots are taken at most this often (ms): once a frame.
const SNAPSHOT_EVERY_MS: u64 = 8;
static TAKEN: AtomicU64 = AtomicU64::new(0);
/// The `HUD_DI` element, once found.
static ROOT: AtomicUsize = AtomicUsize::new(0);
/// Set while a snapshot runs: the engine calls it makes come through the same hooks.
static BUSY: AtomicBool = AtomicBool::new(false);

type ThisFn = unsafe extern "system" fn(*mut c_void) -> usize;
type BoolFn = unsafe extern "system" fn(*mut c_void) -> bool;
type OutFn = unsafe extern "system" fn(*mut c_void, *mut f32) -> *mut f32;
type IndexFn = unsafe extern "system" fn(*mut c_void, i32) -> usize;
type CountFn = unsafe extern "system" fn(*mut c_void) -> i32;
type FloatFn = unsafe extern "system" fn(*mut c_void) -> f32;
type SetFloatFn = unsafe extern "system" fn(*mut c_void, f32);
type SetTextFn = unsafe extern "system" fn(*mut c_void, *const c_void);

static TRANSFORM_ORIGINAL: Original<OutFn> = Original::new();
static SET_OPACITY_ORIGINAL: Original<SetFloatFn> = Original::new();
/// The panel widgets' engine objects, by panel, for [`set_opacity`] (0: none known).
static HELD: [AtomicUsize; WIDGETS.len()] = [const { AtomicUsize::new(0) }; WIDGETS.len()];
/// Fades of panel widgets the game asked for and got full opacity instead, and the corrections
/// [`hold_opaque`] still had to make (a fade that went around the setter).
static FADES_HELD: AtomicU64 = AtomicU64::new(0);
static CORRECTED: AtomicU64 = AtomicU64::new(0);
static ACTUALLY_VISIBLE_ORIGINAL: Original<BoolFn> = Original::new();
static SET_TEXT_ORIGINAL: Original<SetTextFn> = Original::new();

/// The engine's UI getters.
struct Calls {
    parent: ThisFn,
    children: CountFn,
    child: IndexFn,
    visible: BoolFn,
    transform: OutFn,
    size: OutFn,
    opacity: FloatFn,
    set_opacity: SetFloatFn,
}
static CALLS: OnceLock<Calls> = OnceLock::new();

const IS_ACTUALLY_VISIBLE: &str = "?IsActuallyVisible@IUIElement@@QEBA_NXZ";
const IS_VISIBLE: &str = "?IsVisible@IUIElement@@QEBA_NXZ";
const GET_GLOBAL_TRANSFORM: &str = "?GetGlobalTransform@IUIElement@@QEAA?AVmtx34@@XZ";
const GET_SIZE: &str = "?GetSize@IUIElement@@UEBA?AVvec3@@XZ";
const GET_OPACITY: &str = "?GetOpacity@IUIElement@@UEBAMXZ";
const SET_OPACITY: &str = "?SetOpacity@IUIElement@@UEAAXM@Z";
const GET_PARENT: &str = "?GetParent@IUIElement@@QEAAPEAV1@XZ";
const GET_CHILDREN_COUNT: &str = "?GetChildrenCount@IUIElement@@QEAAHXZ";
const GET_CHILDREN: &str = "?GetChildren@IUIElement@@QEAAPEAV1@H@Z";
const SET_TEXT: &str = "?SetText@IUIText@@UEAAXAEBV?$string_base@D@ttl@@@Z";

/// Looks the engine's UI functions up and hooks the three the game calls on its elements.
pub fn install(hooks: &mut Hooks, engine: &Module) -> Result<(), Rejection> {
    let find = |name: &str| engine.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    // SAFETY: each export has the signature its mangled name states, on x64 (a class returned by
    // value comes back through a hidden pointer, the second argument).
    let calls = unsafe {
        Calls {
            parent: std::mem::transmute::<usize, ThisFn>(find(GET_PARENT)?),
            children: std::mem::transmute::<usize, CountFn>(find(GET_CHILDREN_COUNT)?),
            child: std::mem::transmute::<usize, IndexFn>(find(GET_CHILDREN)?),
            visible: std::mem::transmute::<usize, BoolFn>(find(IS_VISIBLE)?),
            transform: std::mem::transmute::<usize, OutFn>(find(GET_GLOBAL_TRANSFORM)?),
            size: std::mem::transmute::<usize, OutFn>(find(GET_SIZE)?),
            opacity: std::mem::transmute::<usize, FloatFn>(find(GET_OPACITY)?),
            set_opacity: std::mem::transmute::<usize, SetFloatFn>(find(SET_OPACITY)?),
        }
    };
    // SAFETY: the detours have the targets' signatures; their prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&TRANSFORM_ORIGINAL, "IUIElement::GetGlobalTransform", find(GET_GLOBAL_TRANSFORM)?, transform as OutFn)?;
        hooks.inline_decoded(&ACTUALLY_VISIBLE_ORIGINAL, "IUIElement::IsActuallyVisible", find(IS_ACTUALLY_VISIBLE)?, actually_visible as BoolFn)?;
        hooks.inline_decoded(&SET_TEXT_ORIGINAL, "IUIText::SetText", find(SET_TEXT)?, set_text as SetTextFn)?;
    }
    // Where the game fades its widgets: the engine object's opacity setter, which the exported one
    // jumps to.
    match internal_setter(find(SET_OPACITY)?) {
        // SAFETY: the setter `(engine object, opacity)`, read from the exported one's jump; the
        // detour has its signature; its prologue is decoded and moved.
        Some(target) => unsafe { hooks.inline_decoded(&SET_OPACITY_ORIGINAL, "CUIElement::SetOpacity", target, set_opacity as SetFloatFn)? },
        None => log!("HUD elements: the opacity setter is not where IUIElement::SetOpacity jumps; the panels are held opaque afterwards only"),
    }
    let _ = CALLS.set(calls);
    Ok(())
}

/// The engine object's opacity setter that `IUIElement::SetOpacity` (`export`) hands over to: its
/// code is `mov rcx, [rcx+0x18]; test rcx, rcx; jne <setter>; ret` (2026-10-07).
fn internal_setter(export: usize) -> Option<usize> {
    let mut code = [0u8; 14];
    if !mem::read_bytes(export, &mut code) || code[..9] != [0x48, 0x8b, 0x49, 0x18, 0x48, 0x85, 0xc9, 0x0f, 0x85] || code[13] != 0xc3 {
        return None;
    }
    let jump = i32::from_le_bytes([code[9], code[10], code[11], code[12]]);
    (export + 13).checked_add_signed(jump as isize)
}

/// The engine object's opacity setter: a panel widget the game fades gets full opacity instead
/// (`world_hud_opaque`). Held only after the game had set it (writing it back each frame raced
/// the game's own fade, and the tool and weapon panels flickered, 2026-10-07).
unsafe extern "system" fn set_opacity(object: *mut c_void, opacity: f32) {
    let _flight = InFlight::enter();
    let held = opacity < 1.0
        && object as usize != 0
        && HELD.iter().any(|h| h.load(Relaxed) == object as usize)
        && crate::view::stereo::capturing()
        && crate::view::stereo::config().world_hud_opaque;
    if held {
        FADES_HELD.fetch_add(1, Relaxed);
    }
    // SAFETY: forwards the game's own call, its opacity held at 1 for a panel widget.
    unsafe { SET_OPACITY_ORIGINAL.get()(object, if held { 1.0 } else { opacity }) }
}

unsafe extern "system" fn transform(element: *mut c_void, out: *mut f32) -> *mut f32 {
    let _flight = InFlight::enter();
    seen(element as usize);
    // SAFETY: forwards the game's own call.
    unsafe { TRANSFORM_ORIGINAL.get()(element, out) }
}

unsafe extern "system" fn actually_visible(element: *mut c_void) -> bool {
    let _flight = InFlight::enter();
    seen(element as usize);
    // SAFETY: forwards the game's own call.
    unsafe { ACTUALLY_VISIBLE_ORIGINAL.get()(element) }
}

unsafe extern "system" fn set_text(element: *mut c_void, text: *const c_void) {
    let _flight = InFlight::enter();
    seen(element as usize);
    // SAFETY: forwards the game's own call.
    unsafe { SET_TEXT_ORIGINAL.get()(element, text) }
}

/// An element the game handed an engine UI function, on the game thread: finds the root from it
/// if none is known, and takes the snapshot when it is due.
fn seen(element: usize) {
    if element == 0 || BUSY.load(Relaxed) || !crate::view::stereo::capturing() {
        return;
    }
    let now = monaka_channel::tick();
    if now.saturating_sub(TAKEN.load(Relaxed)) < SNAPSHOT_EVERY_MS {
        return;
    }
    let Some(calls) = CALLS.get() else { return };
    BUSY.store(true, Relaxed);
    if !is_root(ROOT.load(Relaxed)) {
        ROOT.store(root_of(calls, element).filter(|&r| is_root(r)).unwrap_or(0), Relaxed);
    }
    let root = ROOT.load(Relaxed);
    if root != 0 {
        TAKEN.store(now, Relaxed);
        let started = std::time::Instant::now();
        let layout = snapshot(calls, root, now);
        note_cost(started.elapsed());
        note_layout(&layout);
        if let Ok(mut latest) = LAYOUT.lock() {
            *latest = Some(Arc::new(layout));
        }
    }
    BUSY.store(false, Relaxed);
}

/// The engine object behind an element, read without calling the engine (the getters read it
/// unchecked): `None` for one without it.
fn engine_object(element: usize) -> Option<usize> {
    mem::read::<usize>(element + 0x18).filter(|&o| o != 0 && mem::read::<usize>(o).is_some())
}

/// An element's name, read without calling the engine (`gui::IObject::GetName`'s own reads).
fn name_of(element: usize) -> Option<String> {
    let holder = mem::read::<usize>(engine_object(element)? + 8)?;
    mem::read_c_string(mem::read::<usize>(holder)?, 96)
}

/// Whether `element` is the HUD's root, checked by reads alone (a level change frees the old one).
fn is_root(element: usize) -> bool {
    element != 0 && name_of(element).as_deref() == Some(ROOT_NAME)
}

fn root_of(calls: &Calls, element: usize) -> Option<usize> {
    let mut at = element;
    for _ in 0..64 {
        engine_object(at)?;
        // SAFETY: the engine's own getter on an element with its engine object (game thread).
        let parent = unsafe { (calls.parent)(at as *mut c_void) };
        if parent == 0 {
            return Some(at);
        }
        at = parent;
    }
    None
}

/// Whether elements of the class at `vtable` are texts (by class name, remembered).
fn is_text(element: usize) -> bool {
    static KNOWN: Mutex<Option<HashMap<usize, bool>>> = Mutex::new(None);
    let Some(vtable) = mem::read::<usize>(element) else { return false };
    let Ok(mut known) = KNOWN.lock() else { return false };
    *known.get_or_insert_with(HashMap::new).entry(vtable).or_insert_with(|| class_name(element).is_some_and(|c| c.contains("Text")))
}

/// One of the root's widgets, with its drawable leaves as last read.
struct Widget {
    element: usize,
    name: String,
    piece: Option<usize>,
    leaves: Vec<Leaf>,
    read: bool,
}

/// The widgets between snapshots: listed again every [`RELIST_EVERY`] snapshots (or for another
/// root), the panels' read every other snapshot, the rest [`ROTATE`] at a time in turn. Read whole
/// each time, the tree cost the game thread 1.6 ms a frame (690 elements at about 2.4 us each,
/// in engine calls: 2026-10-07); the rest only tells where a panel's draws end.
struct Cache {
    root: usize,
    widgets: Vec<Widget>,
    next: usize,
    snapshots: u64,
}
/// An element's box on screen (left, top, right, bottom, layout pixels), from its global transform
/// and size; none when it reads as nothing.
fn element_box(calls: &Calls, element: usize) -> Option<[f32; 4]> {
    let mut m = [0.0f32; 12];
    let mut size = [0.0f32; 4];
    // SAFETY: the engine's own getters on an element of the tree (game thread); the outputs are
    // locals of the size it writes (mtx34: 12 floats, vec3: 3).
    unsafe {
        (calls.transform)(element as *mut c_void, m.as_mut_ptr());
        (calls.size)(element as *mut c_void, size.as_mut_ptr());
    }
    let (at, far) = ([m[3], m[7]], [m[3] + size[0] * m[0], m[7] + size[1] * m[5]]);
    let b = [at[0].min(far[0]), at[1].min(far[1]), at[0].max(far[0]), at[1].max(far[1])];
    (b.iter().all(|v| v.is_finite()) && b[2] - b[0] > 1.0 && b[3] - b[1] > 1.0).then_some(b)
}

static CACHE: Mutex<Cache> = Mutex::new(Cache { root: 0, widgets: Vec::new(), next: 0, snapshots: 0 });
const RELIST_EVERY: u64 = 64;
const ROTATE: usize = 4;

/// The drawable leaves under `root`, each with its widget's panel, and the panel widgets' boxes.
fn snapshot(calls: &Calls, root: usize, now: u64) -> Layout {
    let mut layout = Layout { taken: now, ..Layout::default() };
    let Ok(mut cache) = CACHE.lock() else { return layout };
    let cache = &mut *cache;
    let n = cache.snapshots;
    cache.snapshots += 1;
    let r = root as *mut c_void;
    // SAFETY: the engine's own getters on the root (checked by its name), on the game thread.
    let opacity = if unsafe { (calls.visible)(r) } { unsafe { (calls.opacity)(r) } } else { 0.0 };
    layout.root = element_box(calls, root);
    if cache.root != root || cache.widgets.is_empty() || n.is_multiple_of(RELIST_EVERY) {
        // SAFETY: as above.
        let count = unsafe { (calls.children)(r) };
        let mut old = std::mem::take(&mut cache.widgets);
        cache.widgets = children(calls, root, count)
            .into_iter()
            .map(|element| {
                let kept = old.iter_mut().find(|w| w.element == element).map(|w| std::mem::take(&mut w.leaves));
                let name = name_of(element).unwrap_or_default();
                let piece = WIDGETS.iter().position(|w| *w == name);
                Widget { element, name, piece, read: kept.is_some(), leaves: kept.unwrap_or_default() }
            })
            .collect();
        cache.root = root;
    }
    if crate::view::stereo::config().world_hud_opaque {
        hold_opaque(calls, &cache.widgets);
    }
    let count = cache.widgets.len().max(1);
    let next = cache.next;
    // A probe's widget is read at every snapshot (`research::widget`).
    let probed: Option<&str> = crate::research::widget::watched();
    for (i, widget) in cache.widgets.iter_mut().enumerate() {
        let watched = probed == Some(widget.name.as_str());
        let due = watched || !widget.read || if widget.piece.is_some() { n.is_multiple_of(2) } else { (i + count - next) % count < ROTATE };
        if due {
            widget.leaves.clear();
            walk(calls, widget.element, widget.piece, opacity, &mut widget.leaves);
            widget.read = true;
            if watched {
                crate::research::widget::note(&widget.name, &widget.leaves);
            }
        }
    }
    cache.next = (next + ROTATE) % count;
    // A leaf outside the HUD's own box shows nothing (now and then a minimap icon sits thousands of
    // pixels off the screen): it would grow its piece's box past the screen for as long as the piece
    // stays, and shrink the piece to a speck on its panel (2026-10-09).
    let root = layout.root;
    let on_screen = |rect: [f32; 4]| root.is_none_or(|r| rect[0] >= r[0] - OFF_SCREEN_SLACK && rect[1] >= r[1] - OFF_SCREEN_SLACK && rect[2] <= r[2] + OFF_SCREEN_SLACK && rect[3] <= r[3] + OFF_SCREEN_SLACK);
    for widget in &cache.widgets {
        for (i, leaf) in widget.leaves.iter().enumerate() {
            if let Some(piece) = leaf.piece.filter(|_| on_screen(leaf.rect)) {
                layout.pieces[piece] = Some(piece_box(piece, layout.pieces[piece], leaf.rect));
            }
            layout.leaves.push(Leaf { last: i + 1 == widget.leaves.len(), ..*leaf });
        }
    }
    layout
}

/// How far past the HUD's own box (layout pixels) a leaf may reach and still count for its piece's
/// box.
const OFF_SCREEN_SLACK: f32 = 8.0;

/// The minimap's piece in [`WIDGETS`] (`HudRadar`).
pub const MINIMAP: usize = 0;

/// A piece's box with one more of its leaves: the union of its leaves, except the minimap's, which
/// is its largest leaf, the radar's frame. The radar also holds the icons of targets beyond its
/// range, placed outside its circle and hidden by its round mask: counted, they grew the box to
/// several times the minimap and left the map small in a corner of its panel (2026-10-09).
fn piece_box(piece: usize, so_far: Option<[f32; 4]>, rect: [f32; 4]) -> [f32; 4] {
    let area = |r: [f32; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
    match so_far {
        None => rect,
        Some(b) if piece == MINIMAP => if area(rect) > area(b) { rect } else { b },
        Some(b) => union(b, rect),
    }
}

/// The panel widgets at full opacity, whatever the game faded them to: the game fades its pieces
/// out (health at full health, the quest list after a while), on the hands they stay. Through the
/// engine's own setter, at every snapshot (the game may fade again each frame). Only the widgets
/// themselves: what the game hides inside one (an empty slot's parts) stays hidden.
fn hold_opaque(calls: &Calls, widgets: &[Widget]) {
    static NOTED: AtomicUsize = AtomicUsize::new(0);
    for widget in widgets {
        let Some(piece) = widget.piece else { continue };
        // Its setter holds it from now on.
        HELD[piece].store(engine_object(widget.element).unwrap_or(0), Relaxed);
        let w = widget.element as *mut c_void;
        // SAFETY: the engine's own getter and setter on a widget of the root (game thread).
        let faded = unsafe { (calls.opacity)(w) };
        if faded < 1.0 {
            // SAFETY: as above.
            unsafe { (calls.set_opacity)(w, 1.0) };
            CORRECTED.fetch_add(1, Relaxed);
            if NOTED.fetch_or(1 << piece, Relaxed) & 1 << piece == 0 {
                log!("HUD elements: {} faded to {faded:.2} by the game; held at full opacity", WIDGETS[piece]);
            }
        }
    }
}

fn children(calls: &Calls, element: usize, count: i32) -> Vec<usize> {
    // SAFETY: the engine's own getter, indices under its count (game thread).
    (0..count.clamp(0, 512)).map(|i| unsafe { (calls.child)(element as *mut c_void, i) }).filter(|&c| engine_object(c).is_some()).collect()
}

/// `element` and what is under it, drawn with `above` times its own opacity: its drawable leaves
/// into `leaves`. Hidden or transparent elements are skipped with all under them.
fn walk(calls: &Calls, element: usize, piece: Option<usize>, above: f32, leaves: &mut Vec<Leaf>) {
    let e = element as *mut c_void;
    // SAFETY: the engine's own getters on an element of the tree (game thread).
    if !unsafe { (calls.visible)(e) } {
        return;
    }
    // SAFETY: as above.
    let opacity = above * unsafe { (calls.opacity)(e) };
    if opacity <= 0.0 {
        return;
    }
    // SAFETY: as above.
    let count = unsafe { (calls.children)(e) };
    if count > 0 {
        for child in children(calls, element, count) {
            walk(calls, child, piece, opacity, leaves);
        }
        return;
    }
    let text = is_text(element);
    // A text with nothing in it draws nothing.
    if text && engine_object(element).and_then(|o| mem::read::<usize>(o + 0x3d8)).is_none_or(|s| s == 0) {
        return;
    }
    let mut m = [0.0f32; 12];
    let mut size = [0.0f32; 4];
    // SAFETY: the engine's own getters; the outputs are locals of the size it writes (mtx34: 12
    // floats, vec3: 3).
    unsafe {
        (calls.transform)(e, m.as_mut_ptr());
        (calls.size)(e, size.as_mut_ptr());
    }
    let at = [m[3], m[7]];
    let far = [at[0] + size[0] * m[0], at[1] + size[1] * m[5]];
    if !at.iter().chain(&far).all(|v| v.is_finite()) {
        return;
    }
    let rect = [at[0].min(far[0]), at[1].min(far[1]), at[0].max(far[0]), at[1].max(far[1])];
    leaves.push(Leaf { piece, text, at, rect, last: false });
}

pub use monaka_core::hud::union;

/// What snapshots cost the game thread: logged after 1, 100 and every 10000 of them.
fn note_cost(took: std::time::Duration) {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    static TOTAL_US: AtomicU64 = AtomicU64::new(0);
    static MAX_US: AtomicU64 = AtomicU64::new(0);
    let us = took.as_micros() as u64;
    let n = COUNT.fetch_add(1, Relaxed) + 1;
    let total = TOTAL_US.fetch_add(us, Relaxed) + us;
    MAX_US.fetch_max(us, Relaxed);
    if n == 1 || n == 100 || n.is_multiple_of(10000) {
        log!(
            "HUD elements: {n} snapshots, {} us each on average, {} us at most; panel fades held at the setter {}, corrected after {}",
            total / n,
            MAX_US.load(Relaxed),
            FADES_HELD.load(Relaxed),
            CORRECTED.load(Relaxed)
        );
    }
}

/// Logs the first snapshot, and which panel widgets come and go.
fn note_layout(layout: &Layout) {
    static FIRST: AtomicBool = AtomicBool::new(true);
    static SHOWN: AtomicUsize = AtomicUsize::new(usize::MAX);
    if FIRST.swap(false, Relaxed) {
        log!("HUD elements: {} drawable leaves under {ROOT_NAME}, {} of them on panels", layout.leaves.len(), layout.leaves.iter().filter(|l| l.piece.is_some()).count());
    }
    let shown = layout.pieces.iter().enumerate().filter(|(_, b)| b.is_some()).fold(0usize, |bits, (i, _)| bits | 1 << i);
    let before = SHOWN.swap(shown, Relaxed);
    if before != shown {
        let names: Vec<&str> = WIDGETS.iter().enumerate().filter(|(i, _)| shown & 1 << i != 0).map(|(_, n)| *n).collect();
        log!("HUD elements: drawn now: {}", if names.is_empty() { "none of the panels' widgets".to_owned() } else { names.join(", ") });
    }
}

/// The leaf an image draw is, by its quad's corner (layout pixels): the next one in tree order
/// from `from` at that corner, else any (the snapshot can be a frame off the draws).
pub fn image_leaf(leaves: &[Leaf], from: usize, corner: [f32; 2]) -> Option<usize> {
    let at = |l: &Leaf| !l.text && (l.at[0] - corner[0]).abs() <= CORNER_TOLERANCE && (l.at[1] - corner[1]).abs() <= CORNER_TOLERANCE;
    (from..leaves.len()).find(|&i| at(&leaves[i])).or_else(|| (0..from.min(leaves.len())).find(|&i| at(&leaves[i])))
}

/// The text leaf a text draw is, by its first glyph's place (layout pixels): one whose row it is
/// on (its top within [`ROW_TOLERANCE`]), across the leaf or up to a leaf's width before it (a
/// right-aligned text longer than its box starts left of it), the nearest row first.
pub fn text_leaf(leaves: &[Leaf], glyph: [f32; 2]) -> Option<usize> {
    leaves
        .iter()
        .enumerate()
        .filter(|(_, l)| l.text && (l.rect[1] - glyph[1]).abs() <= ROW_TOLERANCE)
        .filter(|(_, l)| glyph[0] >= l.rect[0] - (l.rect[2] - l.rect[0]) - ROW_TOLERANCE && glyph[0] <= l.rect[2] + ROW_TOLERANCE)
        .min_by(|(_, a), (_, b)| (a.rect[1] - glyph[1]).abs().total_cmp(&(b.rect[1] - glyph[1]).abs()))
        .map(|(i, _)| i)
}

/// An image draw's corner lies on its leaf's within this (layout pixels; measured: exact to the
/// tenth logged).
const CORNER_TOLERANCE: f32 = 1.0;
/// A text's first glyph lies on its leaf's row within this (layout pixels; measured: 0 to 3.2,
/// the shadow and outline passes a pixel or two off the text).
const ROW_TOLERANCE: f32 = 6.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_minimap_is_its_frame_and_the_rest_their_leaves_union() {
        let frame = [3100.0, 78.0, 3585.0, 561.0];
        let icon = [3300.0, 300.0, 3332.0, 332.0];
        let far_icon = [2500.0, 900.0, 2532.0, 932.0];
        // The minimap: the frame, whatever order the leaves come in; a far icon does not grow it.
        let mut b = piece_box(MINIMAP, None, icon);
        for leaf in [frame, far_icon, icon] {
            b = piece_box(MINIMAP, Some(b), leaf);
        }
        assert_eq!(b, frame);
        // Another piece: the union.
        assert_eq!(piece_box(2, Some(frame), far_icon), [2500.0, 78.0, 3585.0, 932.0]);
    }

    /// Leaves of the 2026-10-07 HUD (logged by a probe): health's medkit back, its count, the quests'
    /// title, text and icon.
    fn leaves() -> Vec<Leaf> {
        let leaf = |piece, text, at: [f32; 2], size: [f32; 2]| Leaf { piece, text, at, rect: [at[0], at[1], at[0] + size[0], at[1] + size[1]], last: false };
        vec![
            leaf(Some(3), false, [185.9, 718.8], [86.8, 33.0]),
            leaf(Some(3), true, [247.9, 718.8], [16.5, 31.0]),
            leaf(Some(3), true, [417.3, 669.3], [16.5, 31.0]),
            leaf(None, false, [1001.8, 685.8], [640.3, 8.3]),
            leaf(Some(2), true, [1901.1, 1109.6], [537.1, 33.0]),
            leaf(Some(2), true, [1892.8, 1142.7], [504.3, 41.8]),
            leaf(Some(2), false, [2403.0, 1146.8], [33.0, 33.0]),
        ]
    }

    #[test]
    fn image_draws_are_the_leaf_at_their_corner() {
        let leaves = leaves();
        // Draw 61 and the quests' icon (draw 102) of that frame.
        assert_eq!(image_leaf(&leaves, 0, [185.9, 718.9]), Some(0));
        assert_eq!(image_leaf(&leaves, 1, [2403.1, 1146.7]), Some(6));
        // From past it, the snapshot a frame off: still found.
        assert_eq!(image_leaf(&leaves, 7, [185.9, 718.9]), Some(0));
        // The minimap's own geometry (draws 2-4): no leaf.
        assert_eq!(image_leaf(&leaves, 0, [1322.8, 1320.7]), None);
        // A text leaf is never an image draw's.
        assert_eq!(image_leaf(&leaves, 0, [247.9, 718.8]), None);
    }

    #[test]
    fn text_draws_are_the_text_leaf_of_their_row() {
        let leaves = leaves();
        // Health's count exactly at its leaf (draw 59).
        assert_eq!(text_leaf(&leaves, [417.3, 669.3]), Some(2));
        // The quest title, right-aligned in its box, its passes 3 pixels up (draws 96-98).
        assert_eq!(text_leaf(&leaves, [2326.0, 1106.4]), Some(4));
        // The quest text, longer than its box, starting 160 pixels left of it (draws 99-101).
        assert_eq!(text_leaf(&leaves, [1731.7, 1144.7]), Some(5));
        // Nothing on that row.
        assert_eq!(text_leaf(&leaves, [100.0, 300.0]), None);
    }
}
