//! A steady flashlight in VR (`flashlight_steady`, default on with stereo).
//!
//! The player's flashlight is a screen-space post-process: the renderer lights and shadows each
//! frame from its render camera, the light's source a little off it (`f_flashlight_pp_offset_source`,
//! 0.2 right and 0.05 down) and swayed by the camera's movement from the frame before
//! (`logic_script.scr`: `f_pp_flashlight_pp_offset_move_x/y` and `_offset_sway`, from the change in
//! the camera's direction and position). In VR the render camera alternates between the eyes and
//! follows the head, so the sway never settles: the light swings with every head movement and
//! shadows move against the player ("the light comes from the cursor", 2026-10-08). The scripts
//! scale the sway by `f_flashlight_pp_sway`, which the game sets through `CVarlist::Set`: here it
//! is held at zero, and the source offset can be set there too (`flashlight_source=x,y`).
//!
//! `probe_flashlight=1` logs the flashlight variables the game sets (each name's first values).

use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original};
use monaka_producer::{Rejection, log};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use std::sync::{Mutex, OnceLock};

/// `void CVarlist::Set(ttl::string_const<char> name, float value)` (engine; not exported): the
/// name by pointer to the string constant (a tagged pointer: the top byte a tag, the rest the
/// characters' address). The engine's build is not pinned: its first bytes are.
const VARLIST_SET: usize = 0xcbc840;
const VARLIST_SET_PROLOGUE: [u8; 16] = [0x48, 0x89, 0x5c, 0x24, 0x10, 0x48, 0x89, 0x6c, 0x24, 0x18, 0x48, 0x89, 0x74, 0x24, 0x20, 0x57];
const ADDRESS: u64 = 0x00ff_ffff_ffff_ffff;
/// What the scripts scale the sway by: the scripts write their own variables directly, but the
/// game sets this, and the source offset, through `CVarlist::Set` (measured 2026-10-08).
const SWAY: &str = "f_flashlight_pp_sway";
const SOURCE: [&str; 2] = ["f_flashlight_pp_offset_source_x", "f_flashlight_pp_offset_source_y"];
/// Names logged by the probe (values and counts).
const PROBED: &str = "f_flashlight";

type SetFn = unsafe extern "C" fn(*mut c_void, *const u64, f32);

static ORIGINAL: Original<SetFn> = Original::new();

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Hold the sway at zero.
    pub steady: bool,
    /// How far the screen-space shadows are pushed out from the light's point on screen (the
    /// game's: 0.25), if set. The game never sets it at run time, so it is written once, beside the
    /// sway.
    pub shadow_offset: Option<f32>,
    /// The flashlight's shadow strength (the game's: 10), if set (written once, beside the sway).
    /// Its shadows are traced in 2D toward the light's point on screen, the screen centre, so each
    /// is thrown straight out from where the player looks; in VR each eye has its own centre and
    /// the head never stops, so they swing and do not fuse (2026-10-08). 0 turns them off.
    pub shadow_scale: Option<f32>,
    /// The light's source offset (the game's: 0.2, -0.05), if set.
    pub source: Option<[f32; 2]>,
    pub probe: bool,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

#[derive(Clone, Copy)]
enum Action {
    Keep,
    Set(f32),
    /// The sway: held at zero (`Some`) or kept (`None`); its variable list also gets the shadow
    /// variables, once.
    Sway(Option<f32>),
    /// Logged (the probe), with its index for the per-name count.
    Log(usize),
}

/// What to do with each name, by its tagged pointer (decided once per name).
static ACTIONS: Mutex<Option<HashMap<u64, Action>>> = Mutex::new(None);
static PROBED_NAMES: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());
static HELD: AtomicU64 = AtomicU64::new(0);
static SHADOW_WRITTEN: AtomicBool = AtomicBool::new(false);
/// The shadow variables' names, as the game's tools setter passes plain C strings (tag 0x11).
static SHADOW_OFFSET_NAME: &[u8] = b"f_flashlight_pp_shadow_offset\0";
static SHADOW_SCALE_NAME: &[u8] = b"f_flashlight_pp_shadow_scale\0";
static INSTALLED: AtomicBool = AtomicBool::new(false);

pub fn install(hooks: &mut Hooks, engine: &Module, settings: Settings) -> Result<(), Rejection> {
    if !engine.bytes_match(VARLIST_SET, &VARLIST_SET_PROLOGUE) {
        return Err(Rejection::revision("the engine's variable setter is not where it was inspected"));
    }
    let target = engine.at(VARLIST_SET);
    let _ = SETTINGS.set(settings);
    // SAFETY: the detour has the export's signature (the name by pointer, the value in xmm2); its
    // prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&ORIGINAL, "CVarlist::Set", target, set as SetFn)? };
    INSTALLED.store(true, Relaxed);
    log!("flashlight: {settings:?}");
    Ok(())
}

/// The name a tagged string constant points at, if it is plain text.
///
/// # Safety
/// `tagged` must be a string constant as the game passes it (its characters NUL-terminated).
unsafe fn text(tagged: u64) -> Option<String> {
    let at = (tagged & ADDRESS) as *const u8;
    if at.is_null() {
        return None;
    }
    let mut name = Vec::with_capacity(48);
    for i in 0..96 {
        // SAFETY: the caller's guarantee; the text ends at its NUL, read no further.
        let byte = unsafe { *at.add(i) };
        if byte == 0 {
            return String::from_utf8(name).ok();
        }
        if !byte.is_ascii_graphic() {
            return None;
        }
        name.push(byte);
    }
    None
}

fn decide(varlist: usize, tagged: u64) -> Action {
    let Some(settings) = SETTINGS.get() else { return Action::Keep };
    // SAFETY: the game's own name for this call.
    let name = unsafe { text(tagged) };
    if settings.probe {
        log!("flashlight probe: varlist {varlist:#x} sets {} (tag {:#x})", name.as_deref().unwrap_or("?"), tagged >> 56);
    }
    let Some(name) = name else { return Action::Keep };
    if name == SWAY {
        if settings.steady {
            log!("flashlight: holding {name} at 0");
        }
        return Action::Sway(settings.steady.then_some(0.0));
    }
    if let Some(source) = settings.source
        && let Some(axis) = SOURCE.iter().position(|s| *s == name)
    {
        log!("flashlight: {name} -> {}", source[axis]);
        return Action::Set(source[axis]);
    }
    if settings.probe && name.starts_with(PROBED) {
        let mut names = PROBED_NAMES.lock().unwrap_or_else(|e| e.into_inner());
        names.push((name, 0));
        return Action::Log(names.len() - 1);
    }
    Action::Keep
}

unsafe extern "C" fn set(varlist: *mut c_void, name: *const u64, value: f32) {
    let _flight = InFlight::enter();
    // SAFETY: the game's string constant, passed by pointer.
    let tagged = if name.is_null() { 0 } else { unsafe { *name } };
    let action = if tagged == 0 {
        Action::Keep
    } else {
        let mut actions = ACTIONS.lock().unwrap_or_else(|e| e.into_inner());
        let map = actions.get_or_insert_with(HashMap::new);
        match map.get(&tagged) {
            Some(action) => *action,
            None => {
                let action = decide(varlist as usize, tagged);
                map.insert(tagged, action);
                action
            }
        }
    };
    let value = match action {
        Action::Keep => value,
        Action::Set(held) => {
            HELD.fetch_add(1, Relaxed);
            held
        }
        Action::Sway(held) => {
            // The sway's variable list also holds the shadow variables: written once, there.
            if let Some(settings) = SETTINGS.get()
                && !SHADOW_WRITTEN.swap(true, Relaxed)
            {
                for (name, value) in [(SHADOW_OFFSET_NAME, settings.shadow_offset), (SHADOW_SCALE_NAME, settings.shadow_scale)] {
                    let Some(value) = value else { continue };
                    let tagged: u64 = (0x11 << 56) | (name.as_ptr() as u64 & ADDRESS);
                    // SAFETY: the game's setter on the game's own variable list, with a name in the
                    // form the game's tools setter passes.
                    unsafe { ORIGINAL.get()(varlist, &tagged, value) };
                    log!("flashlight: {} -> {value}", String::from_utf8_lossy(&name[..name.len() - 1]));
                }
            }
            match held {
                Some(held) => {
                    HELD.fetch_add(1, Relaxed);
                    held
                }
                None => value,
            }
        }
        Action::Log(index) => {
            let mut names = PROBED_NAMES.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((text, seen)) = names.get_mut(index) {
                *seen += 1;
                if *seen <= 3 || seen.is_power_of_two() {
                    log!("flashlight probe: {text} = {value} (set {seen}x)");
                }
            }
            value
        }
    };
    // SAFETY: the original, with the game's name and the value to keep.
    unsafe { ORIGINAL.get()(varlist, name, value) }
}

pub fn report() {
    if INSTALLED.load(Relaxed) {
        let names = ACTIONS.lock().ok().and_then(|a| a.as_ref().map(HashMap::len)).unwrap_or(0);
        log!("flashlight: {} sets held, {names} variable names seen", HELD.load(Relaxed));
    }
}
