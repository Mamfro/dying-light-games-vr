//! Part of the HUD pieces provider ([`crate::providers::hud_pieces`]): the HUD as the game knows
//! it, to tell which piece each HUD draw is.
//!
//! The game's HUD manager (`HUDDIManager`) keeps its HUD components by id ([`COMPONENTS`]), each
//! one the `IUIElement` of its piece of the HUD (the radar, the weapon indicators, the quest
//! objectives, health), and updates them every frame. Right after that update, on the game thread,
//! the HUD's tree is read through the engine's own exported `IUIElement` functions ([`Layout`]):
//! the drawable leaves of every widget of the HUD's root (`HUD_DI`), in tree order, each with its
//! widget's panel (the panels' components are widgets of it), and each piece's box (its leaves'
//! union). The renderer replays the frame's recorded UI commands later, on another thread, so a
//! draw itself carries no element; but the draws follow the leaves, and each says where it is: an
//! image draw's matrix puts its quad's corner exactly on its leaf's global position; a text draw's
//! first glyph sits on its text leaf's row ([`image_leaf`], [`text_leaf`]).
//!
//! Each `IUIElement` wraps an engine object at +0x18 (its name: the string `[[object+8]]`).
//! Positions are layout pixels, in the space of the HUD root's box ([`Layout::root`]).

use crate::providers::engine;
use monaka_core::hud::union;
use monaka_hook::module::Module;
use monaka_hook::probe::class_name;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

/// A panel's HUD component: its id in the HUD manager, and the name of the widget the HUD's layout
/// shows it as (the component itself, or a wrapper around it: [`shown_element`]).
pub struct Component {
    pub id: usize,
    pub widget: &'static str,
}

/// The components each panel shows, in the panels' order (minimap, weapon, quests, health, tool).
pub const COMPONENTS: [Component; 5] = [
    Component { id: 0x28, widget: "HudRadar" },
    Component { id: 0x10, widget: "HudPrimaryWeaponIndicator" },
    Component { id: 0x59, widget: "StackObjectives" },
    Component { id: 0x15, widget: "health_wrap" },
    Component { id: 0x11, widget: "HudSecondaryWeaponIndicator" },
];

/// One drawable leaf: the panel its widget is on (`None`: none, the HUD in the eye keeps it), a
/// text or not, its corner (the global transform's place), its own box and the box it is drawn in
/// (left, top, right, bottom; a text's takes in its characters, wherever they are drawn), in
/// layout pixels; and whether it is its widget's last (a draw after it is another widget's).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Leaf {
    pub piece: Option<usize>,
    pub text: bool,
    pub at: [f32; 2],
    pub rect: [f32; 4],
    pub drawn: [f32; 4],
    pub last: bool,
}

/// The HUD's drawable leaves in tree order (the order they are drawn), and each panel's box (the
/// union of its leaves; `None`: nothing of it is drawn).
#[derive(Debug, Default)]
pub struct Layout {
    pub leaves: Vec<Leaf>,
    pub pieces: [Option<[f32; 4]>; COMPONENTS.len()],
    /// The root's own box (left, top, right, bottom, layout pixels): the space the boxes are in,
    /// which the HUD's draws map onto their viewport. Not the back buffer's size: with FSR the
    /// game renders each eye smaller and lays the HUD out at the screen's size.
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

type ThisFn = unsafe extern "system" fn(*mut c_void) -> usize;
type BoolFn = unsafe extern "system" fn(*mut c_void) -> bool;
type OutFn = unsafe extern "system" fn(*mut c_void, *mut f32) -> *mut f32;
type IndexFn = unsafe extern "system" fn(*mut c_void, i32) -> usize;
type CountFn = unsafe extern "system" fn(*mut c_void) -> i32;
type FloatFn = unsafe extern "system" fn(*mut c_void) -> f32;
type SetFloatFn = unsafe extern "system" fn(*mut c_void, f32);
type UpdateFn = unsafe extern "system" fn(*mut c_void);

static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
static SET_OPACITY_ORIGINAL: Original<SetFloatFn> = Original::new();
/// The panel widgets' engine objects, by panel, for [`set_opacity`] (0: none known).
static HELD: [AtomicUsize; COMPONENTS.len()] = [const { AtomicUsize::new(0) }; COMPONENTS.len()];
/// Fades of panel widgets the game asked for and got full opacity instead, and the corrections
/// [`hold_opaque`] still had to make (a fade that went around the setter).
static FADES_HELD: AtomicU64 = AtomicU64::new(0);
static CORRECTED: AtomicU64 = AtomicU64::new(0);

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
    text: ThisFn,
    text_place: OutFn,
    text_extent: OutFn,
}
static CALLS: OnceLock<Calls> = OnceLock::new();

const IS_VISIBLE: &str = "?IsVisible@IUIElement@@QEBA_NXZ";
const GET_GLOBAL_TRANSFORM: &str = "?GetGlobalTransform@IUIElement@@QEAA?AVmtx34@@XZ";
const GET_SIZE: &str = "?GetSize@IUIElement@@UEBA?AVvec3@@XZ";
const GET_OPACITY: &str = "?GetOpacity@IUIElement@@UEBAMXZ";
const SET_OPACITY: &str = "?SetOpacity@IUIElement@@UEAAXM@Z";
const GET_PARENT: &str = "?GetParent@IUIElement@@QEAAPEAV1@XZ";
const GET_CHILDREN_COUNT: &str = "?GetChildrenCount@IUIElement@@QEAAHXZ";
const GET_CHILDREN: &str = "?GetChildren@IUIElement@@QEAAPEAV1@H@Z";
/// A text's string (`const string_base<char>&`, its character pointer first: null when empty).
const GET_TEXT: &str = "?GetText@IUIText@@UEAAAEBV?$string_base@D@ttl@@XZ";
/// Where a text's characters start against the element's own place, and how far they reach.
const GET_TEXT_POS: &str = "?GetTextPos@IUIText@@QEBA?AVvec3@@XZ";
const GET_TEXT_EXTENT: &str = "?GetTextExtent@IUIText@@QEAA?AVvec3@@XZ";
/// The engine objects of text elements (a progress text is a text).
const TEXT_CLASSES: [&str; 2] = [".?AVCUIText@@", ".?AVCUIProgressText@@"];

/// Looks the engine's UI functions up, hooks the HUD manager's update in the game DLL and the
/// engine object's opacity setter.
pub fn install(hooks: &mut Hooks, engine_module: &Module, gamedll: &Module) -> Result<(), Rejection> {
    let find = |name: &str| engine_module.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
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
            text: std::mem::transmute::<usize, ThisFn>(find(GET_TEXT)?),
            text_place: std::mem::transmute::<usize, OutFn>(find(GET_TEXT_POS)?),
            text_extent: std::mem::transmute::<usize, OutFn>(find(GET_TEXT_EXTENT)?),
        }
    };
    // Before the hooks: the update's detour reads them.
    let _ = CALLS.set(calls);
    // SAFETY: the detour has the update's signature `(manager)`; the game DLL's build is checked by
    // the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&UPDATE_ORIGINAL, "HUD manager update", gamedll.at(engine::HUD_UPDATE), update as UpdateFn)? };
    // Where the game fades its widgets: the engine object's opacity setter, which the exported one
    // jumps to.
    match internal_setter(find(SET_OPACITY)?) {
        // SAFETY: the setter `(engine object, opacity)`, read from the exported one's jump; the
        // detour has its signature; its prologue is decoded and moved.
        Some(target) => unsafe { hooks.inline_decoded(&SET_OPACITY_ORIGINAL, "CUIElement::SetOpacity", target, set_opacity as SetFloatFn)? },
        None => log!("HUD elements: the opacity setter is not where IUIElement::SetOpacity jumps; the panels are held opaque afterwards only"),
    }
    Ok(())
}

/// The engine object's opacity setter that `IUIElement::SetOpacity` (`export`) hands over to: its
/// code is `mov rcx, [rcx+0x18]; test rcx, rcx; jne <setter>; ret`.
fn internal_setter(export: usize) -> Option<usize> {
    let mut code = [0u8; 14];
    if !mem::read_bytes(export, &mut code) || code[..9] != [0x48, 0x8b, 0x49, 0x18, 0x48, 0x85, 0xc9, 0x0f, 0x85] || code[13] != 0xc3 {
        return None;
    }
    let jump = i32::from_le_bytes([code[9], code[10], code[11], code[12]]);
    (export + 13).checked_add_signed(jump as isize)
}

/// The engine object's opacity setter: a panel widget the game fades gets full opacity instead
/// (`world_hud_opaque`). Held as the game sets it: writing it back afterwards each frame races
/// the game's own fade, and the tool and weapon panels flicker.
unsafe extern "system" fn set_opacity(object: *mut c_void, opacity: f32) {
    let _flight = InFlight::enter();
    let held = opacity < 1.0
        && object as usize != 0
        && HELD.iter().any(|h| h.load(Relaxed) == object as usize)
        && crate::providers::eye_site::capturing()
        && crate::providers::eye_site::config().world_hud_opaque;
    if held {
        FADES_HELD.fetch_add(1, Relaxed);
    }
    // SAFETY: forwards the game's own call, its opacity held at 1 for a panel widget.
    unsafe { SET_OPACITY_ORIGINAL.get()(object, if held { 1.0 } else { opacity }) }
}

/// The HUD manager's update: its components just updated, the snapshot is taken when due.
unsafe extern "system" fn update(manager: *mut c_void) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(manager) };
    if !crate::providers::eye_site::capturing() {
        return;
    }
    // A loading screen: a level is being loaded or unloaded and its UI freed. Reading the cached
    // tree then reads freed elements, so it is let go and read afresh once the world is drawn.
    if !monaka_stereo::eye_site::world_drawn_recently::<crate::Dl1>() {
        forget_tree();
        return;
    }
    let now = monaka_channel::tick();
    if now.saturating_sub(TAKEN.load(Relaxed)) < SNAPSHOT_EVERY_MS {
        return;
    }
    let Some(calls) = CALLS.get() else { return };
    TAKEN.store(now, Relaxed);
    let started = std::time::Instant::now();
    let layout = snapshot(calls, manager as usize, now);
    note_cost(started.elapsed());
    note_layout(&layout);
    if let Ok(mut latest) = LAYOUT.lock() {
        *latest = Some(Arc::new(layout));
    }
}

/// Lets go of everything cached of the HUD's tree (on the game thread, in [`update`]).
fn forget_tree() {
    let forgot = CACHE.lock().is_ok_and(|mut cache| {
        let had = cache.root != 0;
        *cache = Cache::EMPTY;
        had
    });
    if forgot {
        if let Ok(mut latest) = LAYOUT.lock() {
            *latest = None;
        }
        static LOGGED: AtomicU64 = AtomicU64::new(0);
        if LOGGED.fetch_add(1, Relaxed) < 20 {
            log!("HUD elements: the world is not drawn (a loading screen): the UI tree is let go until it is");
        }
    }
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

/// An element's parent, if it has one with its engine object.
fn parent_of(calls: &Calls, element: usize) -> Option<usize> {
    // SAFETY: the engine's own getter on an element with its engine object (game thread).
    let parent = unsafe { (calls.parent)(element as *mut c_void) };
    (parent != 0 && engine_object(parent).is_some()).then_some(parent)
}

/// The element panel `piece` shows for its registered `component`: the component itself, or its
/// parent when the parent bears the panel's widget name (a wrapper the HUD's layout groups the piece
/// in). Logged once a run per panel.
fn shown_element(calls: &Calls, piece: usize, component: usize) -> usize {
    static NOTED: AtomicUsize = AtomicUsize::new(0);
    let widget = COMPONENTS[piece].widget;
    let name = name_of(component);
    let parent = parent_of(calls, component);
    let parent_name = parent.and_then(name_of);
    let shown = match parent {
        Some(parent) if name.as_deref() != Some(widget) && parent_name.as_deref() == Some(widget) => parent,
        _ => component,
    };
    if NOTED.fetch_or(1 << piece, Relaxed) & 1 << piece == 0 {
        log!(
            "HUD elements: panel widget {widget}: HUD component {:#x} is a {} named {name:?}, under {parent_name:?}; the panel shows {}",
            COMPONENTS[piece].id,
            class_name(component).unwrap_or_default(),
            if shown == component { "the component" } else { "its parent" }
        );
    }
    shown
}

/// What `element` takes from the elements above it: the product of their opacities (0 when one of
/// them is hidden), and the top of its tree (the HUD's root).
fn above(calls: &Calls, element: usize) -> Option<(f32, usize)> {
    let mut opacity = 1.0;
    let mut at = element;
    for _ in 0..64 {
        let Some(parent) = parent_of(calls, at) else { return Some((opacity, at)) };
        let p = parent as *mut c_void;
        // SAFETY: the engine's own getters on an element of the tree (game thread).
        opacity *= if unsafe { (calls.visible)(p) } { unsafe { (calls.opacity)(p) } } else { 0.0 };
        at = parent;
    }
    None
}

/// Whether the engine object `object` is a text's (by its class, remembered by vtable).
fn is_text(object: usize) -> bool {
    static KNOWN: Mutex<Option<HashMap<usize, bool>>> = Mutex::new(None);
    let Some(vtable) = mem::read::<usize>(object) else { return false };
    let Ok(mut known) = KNOWN.lock() else { return false };
    *known.get_or_insert_with(HashMap::new).entry(vtable).or_insert_with(|| class_name(object).is_some_and(|c| TEXT_CLASSES.contains(&c.as_str())))
}

/// An element's box on screen (left, top, right, bottom, layout pixels), from its global transform
/// and size; none when it reads as nothing.
fn element_box(calls: &Calls, element: usize) -> Option<[f32; 4]> {
    let (at, scale, size) = placement(calls, element);
    let b = corners(at, [at[0] + size[0] * scale[0], at[1] + size[1] * scale[1]]);
    (b.iter().all(|v| v.is_finite()) && b[2] - b[0] > 1.0 && b[3] - b[1] > 1.0).then_some(b)
}

/// Where an element is drawn: its corner, its scale (x, y) and its size, from its global transform
/// and size.
fn placement(calls: &Calls, element: usize) -> ([f32; 2], [f32; 2], [f32; 2]) {
    let mut m = [0.0f32; 12];
    let mut size = [0.0f32; 4];
    // SAFETY: the engine's own getters on an element of the tree (game thread); the outputs are
    // locals of the size it writes (mtx34: 12 floats, vec3: 3).
    unsafe {
        (calls.transform)(element as *mut c_void, m.as_mut_ptr());
        (calls.size)(element as *mut c_void, size.as_mut_ptr());
    }
    ([m[3], m[7]], [m[0], m[5]], [size[0], size[1]])
}

/// The box two opposite corners span (left, top, right, bottom).
fn corners(a: [f32; 2], b: [f32; 2]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])]
}

/// One of the root's widgets, with its drawable leaves as last read.
struct Widget {
    element: usize,
    name: String,
    leaves: Vec<Leaf>,
    read: bool,
}

/// The widgets between snapshots: listed again every [`RELIST_EVERY`] snapshots (or for another
/// root), the panels' read every other snapshot, the rest [`ROTATE`] at a time in turn. Read whole
/// each time, the tree costs the game thread over a millisecond a frame (hundreds of elements,
/// each read through engine calls); the rest only tells where a panel's draws end.
struct Cache {
    root: usize,
    widgets: Vec<Widget>,
    next: usize,
    snapshots: u64,
}

impl Cache {
    const EMPTY: Cache = Cache { root: 0, widgets: Vec::new(), next: 0, snapshots: 0 };
}

static CACHE: Mutex<Cache> = Mutex::new(Cache::EMPTY);
const RELIST_EVERY: u64 = 64;
const ROTATE: usize = 4;

/// The drawable leaves of the HUD's root (above the panels' components under `manager`, the HUD
/// manager), each with its widget's panel, and the panels' boxes.
fn snapshot(calls: &Calls, manager: usize, now: u64) -> Layout {
    let mut layout = Layout { taken: now, ..Layout::default() };
    let opaque = crate::providers::eye_site::config().world_hud_opaque;
    // Each panel's widget (0: its component is not registered), and the root above them.
    let mut shown = [0usize; COMPONENTS.len()];
    let mut root = None;
    for (piece, component) in COMPONENTS.iter().enumerate() {
        let registered = mem::read::<usize>(manager + engine::HUD_COMPONENTS + component.id * 8).filter(|&c| c != 0 && engine_object(c).is_some());
        let Some(registered) = registered else {
            HELD[piece].store(0, Relaxed);
            continue;
        };
        let element = shown_element(calls, piece, registered);
        let Some((_, top)) = above(calls, element) else { continue };
        root.get_or_insert(top);
        if opaque {
            hold_opaque(calls, piece, element);
        }
        shown[piece] = element;
    }
    let Some(root) = root else { return layout };
    layout.root = element_box(calls, root);
    let Ok(mut cache) = CACHE.lock() else { return layout };
    let cache = &mut *cache;
    let n = cache.snapshots;
    cache.snapshots += 1;
    let r = root as *mut c_void;
    // SAFETY: the engine's own getters on the HUD's root, on the game thread.
    let opacity = if unsafe { (calls.visible)(r) } { unsafe { (calls.opacity)(r) } } else { 0.0 };
    if cache.root != root || cache.widgets.is_empty() || n.is_multiple_of(RELIST_EVERY) {
        // SAFETY: as above.
        let count = unsafe { (calls.children)(r) };
        let mut old = std::mem::take(&mut cache.widgets);
        cache.widgets = children(calls, root, count)
            .into_iter()
            .map(|element| {
                let kept = old.iter_mut().find(|w| w.element == element).map(|w| std::mem::take(&mut w.leaves));
                let name = name_of(element).unwrap_or_default();
                Widget { element, name, read: kept.is_some(), leaves: kept.unwrap_or_default() }
            })
            .collect();
        cache.root = root;
    }
    // A widget's panel: the one whose component it shows, else by its name.
    let piece_of = |w: &Widget| shown.iter().position(|&e| e != 0 && e == w.element).or_else(|| COMPONENTS.iter().position(|c| c.widget == w.name));
    let count = cache.widgets.len().max(1);
    let next = cache.next;
    // A probe's widget is read at every snapshot (`research::widget`).
    let probed: Option<&str> = crate::research::widget::watched();
    for (i, widget) in cache.widgets.iter_mut().enumerate() {
        let watched = probed == Some(widget.name.as_str());
        let due = watched || !widget.read || if piece_of(widget).is_some() { n.is_multiple_of(2) } else { (i + count - next) % count < ROTATE };
        if due {
            widget.leaves.clear();
            walk(calls, widget.element, opacity, &mut widget.leaves);
            widget.read = true;
            if watched {
                crate::research::widget::note(&widget.name, &widget.leaves);
            }
        }
    }
    cache.next = (next + ROTATE) % count;
    // A leaf outside the HUD's own box shows nothing (now and then a minimap icon sits thousands of
    // pixels off the screen): it would grow its piece's box past the screen for as long as the piece
    // stays, and shrink the piece to a speck on its panel.
    let bounds = layout.root;
    let on_screen = |r: [f32; 4]| bounds.is_none_or(|b| r[0] >= b[0] - OFF_SCREEN_SLACK && r[1] >= b[1] - OFF_SCREEN_SLACK && r[2] <= b[2] + OFF_SCREEN_SLACK && r[3] <= b[3] + OFF_SCREEN_SLACK);
    for widget in &cache.widgets {
        let piece = piece_of(widget);
        for (i, leaf) in widget.leaves.iter().enumerate() {
            if let Some(piece) = piece.filter(|_| on_screen(leaf.drawn)) {
                layout.pieces[piece] = Some(piece_box(piece, layout.pieces[piece], leaf.drawn));
            }
            layout.leaves.push(Leaf { piece, last: i + 1 == widget.leaves.len(), ..*leaf });
        }
    }
    layout
}

/// How far past the HUD's own box (layout pixels) a leaf may reach and still count for its piece's
/// box.
const OFF_SCREEN_SLACK: f32 = 8.0;

/// The minimap's piece in [`COMPONENTS`] (the radar).
pub const MINIMAP: usize = 0;

/// A piece's box with one more of its leaves: the union of its leaves, except the minimap's, which
/// is its largest leaf, the radar's frame. The radar also holds the icons of targets beyond its
/// range, placed outside its circle and hidden by its round mask: counted, they would grow the box
/// to several times the minimap and leave the map small in a corner of its panel.
fn piece_box(piece: usize, so_far: Option<[f32; 4]>, rect: [f32; 4]) -> [f32; 4] {
    let area = |r: [f32; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
    match so_far {
        None => rect,
        Some(b) if piece == MINIMAP => if area(rect) > area(b) { rect } else { b },
        Some(b) => union(b, rect),
    }
}

/// A panel widget at full opacity, whatever the game faded it to: the game fades its pieces out
/// (health at full health, the quest list after a while), on the hands they stay. Through the
/// engine's own setter, at every snapshot (the game may fade again each frame). Only the widget
/// itself: what the game hides inside one (an empty slot's parts) stays hidden.
fn hold_opaque(calls: &Calls, piece: usize, element: usize) {
    static NOTED: AtomicUsize = AtomicUsize::new(0);
    // Its setter holds it from now on.
    HELD[piece].store(engine_object(element).unwrap_or(0), Relaxed);
    let w = element as *mut c_void;
    // SAFETY: the engine's own getter and setter on a panel's element (game thread).
    let faded = unsafe { (calls.opacity)(w) };
    if faded < 1.0 {
        // SAFETY: as above.
        unsafe { (calls.set_opacity)(w, 1.0) };
        CORRECTED.fetch_add(1, Relaxed);
        if NOTED.fetch_or(1 << piece, Relaxed) & 1 << piece == 0 {
            log!("HUD elements: {} faded to {faded:.2} by the game; held at full opacity", COMPONENTS[piece].widget);
        }
    }
}

fn children(calls: &Calls, element: usize, count: i32) -> Vec<usize> {
    // SAFETY: the engine's own getter, indices under its count (game thread).
    (0..count.clamp(0, 512)).map(|i| unsafe { (calls.child)(element as *mut c_void, i) }).filter(|&c| engine_object(c).is_some()).collect()
}

/// `element` and what is under it, drawn with `above` times its own opacity: its drawable leaves
/// into `leaves` (their panel left for the snapshot to set). Hidden or transparent elements are
/// skipped with all under them.
fn walk(calls: &Calls, element: usize, above: f32, leaves: &mut Vec<Leaf>) {
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
    let Some(object) = engine_object(element) else { return };
    // SAFETY: as above.
    let count = unsafe { (calls.children)(e) };
    if count > 0 {
        for child in children(calls, element, count) {
            walk(calls, child, opacity, leaves);
        }
        return;
    }
    let text = is_text(object);
    // A text with nothing in it draws nothing.
    // SAFETY: the engine's own getter on a text element; it returns its string, character pointer first.
    if text && mem::read::<usize>(unsafe { (calls.text)(e) }).is_none_or(|s| s == 0) {
        return;
    }
    let (at, scale, size) = placement(calls, element);
    let rect = corners(at, [at[0] + size[0] * scale[0], at[1] + size[1] * scale[1]]);
    if !rect.iter().all(|v| v.is_finite()) {
        return;
    }
    // A text drawn longer than its box (right-aligned, it starts left of it; wrapped, its lines go
    // below it) takes in where its characters are.
    let drawn = match text.then(|| text_box(calls, element, at, scale, rect)).flatten() {
        Some(characters) => union(rect, characters),
        None => rect,
    };
    leaves.push(Leaf { piece: None, text, at, rect, drawn, last: false });
}

/// Where a text's characters are drawn (left, top, right, bottom, layout pixels): where they start
/// against the element's corner `at`, and how far they reach, at the element's `scale`. The first
/// few are logged beside the element's own box `rect`.
fn text_box(calls: &Calls, element: usize, at: [f32; 2], scale: [f32; 2], rect: [f32; 4]) -> Option<[f32; 4]> {
    let mut place = [0.0f32; 4];
    let mut extent = [0.0f32; 4];
    // SAFETY: the engine's own getters on a text element (game thread); the outputs are locals of
    // the size they write (vec3: 3 floats).
    unsafe {
        (calls.text_place)(element as *mut c_void, place.as_mut_ptr());
        (calls.text_extent)(element as *mut c_void, extent.as_mut_ptr());
    }
    let start = [at[0] + place[0] * scale[0], at[1] + place[1] * scale[1]];
    let characters = corners(start, [start[0] + extent[0] * scale[0], start[1] + extent[1] * scale[1]]);
    static LOGGED: AtomicU64 = AtomicU64::new(0);
    if LOGGED.fetch_add(1, Relaxed) < TEXTS_LOGGED {
        log!(
            "HUD elements: a text's box {rect:.1?} (scale {scale:.3?}): its characters start {:.1?} from its corner and reach {:.1?}, drawn at {characters:.1?}",
            [place[0], place[1]],
            [extent[0], extent[1]]
        );
    }
    (characters.iter().all(|v| v.is_finite()) && characters[2] > characters[0] && characters[3] > characters[1]).then_some(characters)
}

/// How many texts' boxes are logged beside their characters' ([`text_box`]).
const TEXTS_LOGGED: u64 = 12;

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
    static FIRST: AtomicU64 = AtomicU64::new(0);
    static SHOWN: AtomicUsize = AtomicUsize::new(usize::MAX);
    if FIRST.fetch_add(1, Relaxed) == 0 {
        log!("HUD elements: {} drawable leaves under the HUD's root, {} of them on panels; the root's box {:?}", layout.leaves.len(), layout.leaves.iter().filter(|l| l.piece.is_some()).count(), layout.root);
    }
    let shown = layout.pieces.iter().enumerate().filter(|(_, b)| b.is_some()).fold(0usize, |bits, (i, _)| bits | 1 << i);
    let before = SHOWN.swap(shown, Relaxed);
    if before != shown {
        let names: Vec<&str> = COMPONENTS.iter().enumerate().filter(|(i, _)| shown & 1 << i != 0).map(|(_, c)| c.widget).collect();
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
/// on (its own box's top within [`ROW_TOLERANCE`]), across the leaf or up to a leaf's width before
/// it (a right-aligned text longer than its box starts left of it), the nearest row first.
pub fn text_leaf(leaves: &[Leaf], glyph: [f32; 2]) -> Option<usize> {
    leaves
        .iter()
        .enumerate()
        .filter(|(_, l)| l.text && (l.rect[1] - glyph[1]).abs() <= ROW_TOLERANCE)
        .filter(|(_, l)| glyph[0] >= l.rect[0] - (l.rect[2] - l.rect[0]) - ROW_TOLERANCE && glyph[0] <= l.rect[2] + ROW_TOLERANCE)
        .min_by(|(_, a), (_, b)| (a.rect[1] - glyph[1]).abs().total_cmp(&(b.rect[1] - glyph[1]).abs()))
        .map(|(i, _)| i)
}

/// An image draw's corner lies on its leaf's within this (layout pixels; the two agree to a
/// tenth).
const CORNER_TOLERANCE: f32 = 1.0;
/// A text's first glyph lies on its leaf's row within this (layout pixels; the glyph is up to
/// about 3 off, the shadow and outline passes a pixel or two off the text).
const ROW_TOLERANCE: f32 = 6.0;

#[cfg(test)]
mod tests;
