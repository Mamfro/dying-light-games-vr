//! `probe_frame_order=1`: in what order and on which threads a frame's steps run, for same-frame
//! stereo: the player camera update (the game thread's
//! `SetView(vec3)`), the scene's start and end, the frame data's fill (its main view camera and
//! viewport) and the renderer's present request (and its caller), each with the game's frame
//! counter. Kept in memory and logged in one go at the end (a line at a time would slow the frame).

use super::options;
use crate::providers::engine::{self, FramePresentFn, PrepareFn, SceneFn};
use monaka_core::sync::Locked;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem, thread_id};
use monaka_producer::{Rejection, log};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

static SCENE_ORIGINAL: Original<SceneFn> = Original::new();
static PREPARE_ORIGINAL: Original<PrepareFn> = Original::new();
static FRAME_PRESENT_ORIGINAL: Original<FramePresentFn> = Original::new();
static ENGINE: AtomicUsize = AtomicUsize::new(0);
/// The player camera and the level's camera, once seen.
static PLAYER_CAMERA: AtomicUsize = AtomicUsize::new(0);
static LEVEL_CAMERA: AtomicUsize = AtomicUsize::new(0);

/// A camera's position (its camera-to-world translation), to millimetres.
fn at(camera: usize) -> String {
    if camera == 0 {
        return "-".to_owned();
    }
    let read = |offset: usize| mem::read::<f32>(camera + engine::CAMERA_INVERSE + offset).unwrap_or(f32::NAN);
    format!("({:.3} {:.3} {:.3})", read(0xc), read(0x1c), read(0x2c))
}

/// Both cameras' positions now.
fn cameras() -> String {
    format!("player {} level {}", at(PLAYER_CAMERA.load(Relaxed)), at(LEVEL_CAMERA.load(Relaxed)))
}

/// Events kept (a few hundred frames' worth), from the first.
const KEPT: usize = 600;

struct Events {
    since: Option<Instant>,
    lines: Vec<String>,
}

static EVENTS: Mutex<Events> = Mutex::new(Events { since: None, lines: Vec::new() });

fn note(what: std::fmt::Arguments) {
    let counter = game_counter();
    let mut events = EVENTS.locked();
    if events.lines.len() >= KEPT {
        return;
    }
    let since = *events.since.get_or_insert_with(Instant::now);
    let line = format!("{:9.3} ms  thread {:6}  frame {counter:?}  {what}", since.elapsed().as_secs_f64() * 1000.0, thread_id());
    events.lines.push(line);
}

/// The game's frame counter now.
fn game_counter() -> Option<u32> {
    let engine = ENGINE.load(Relaxed);
    let game = mem::read::<usize>(engine + engine::GAME_GLOBAL).filter(|&g| g != 0)?;
    mem::read::<u32>(game + engine::GAME_COUNTER)
}

/// The player camera update set `camera` (the renderer's `CCamera`).
pub fn player_update(camera: usize) {
    if options().frame_order {
        PLAYER_CAMERA.store(camera, Relaxed);
        note(format_args!("player camera update, camera {camera:#x}; before it {}", cameras()));
    }
}

/// The player camera update has set the camera.
pub fn player_updated() {
    if options().frame_order {
        note(format_args!("player camera set: {}", cameras()));
    }
}

unsafe extern "system" fn scene(game: usize, token: u32, a: usize, b: usize) -> usize {
    let _flight = InFlight::enter();
    note(format_args!("scene start (token {token:#x}, a {a:#x}, b {b:#x}); {}", cameras()));
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { SCENE_ORIGINAL.get()(game, token, a, b) };
    note(format_args!("scene end; {}", cameras()));
    result
}

unsafe extern "system" fn prepare(game: usize, data: usize) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { PREPARE_ORIGINAL.get()(game, data) };
    let camera = mem::read::<usize>(data + engine::FRAME_MAIN_VIEW).unwrap_or(0);
    let viewport = mem::read::<usize>(data + engine::DATA_VIEWPORT).unwrap_or(0);
    LEVEL_CAMERA.store(camera, Relaxed);
    note(format_args!("prepare: main view camera {camera:#x} (level {:#x}), viewport {viewport:#x}; {}", camera.wrapping_sub(engine::LEVEL_CAMERA), cameras()));
    result
}

monaka_hook::caller_shim!(frame_present_stub => frame_present);

unsafe extern "system" fn frame_present(renderer: usize, a: usize, b: usize, caller: usize) -> usize {
    let _flight = InFlight::enter();
    let engine = ENGINE.load(Relaxed);
    let normal = caller == engine + engine::NORMAL_PRESENT_CALLER;
    note(format_args!("frame present request start ({}); {}", if normal { "the frame's own".to_owned() } else { format!("caller {caller:#x}") }, cameras()));
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { FRAME_PRESENT_ORIGINAL.get()(renderer, a, b) };
    note(format_args!("frame present request end"));
    result
}

/// The probe's hooks on the engine and the renderer.
///
/// # Safety
/// `engine` and `renderer` are the game's live modules (each site's prologue is checked).
pub unsafe fn install(hooks: &mut Hooks, engine_module: &Module, renderer: &Module) -> Result<(), Rejection> {
    ENGINE.store(engine_module.base(), Relaxed);
    hooks.site(&SCENE_ORIGINAL, engine_module, &engine::SCENE, scene as SceneFn)?;
    hooks.site(&PREPARE_ORIGINAL, engine_module, &engine::PREPARE, prepare as PrepareFn)?;
    // SAFETY: the shim stub takes the hooked function's three arguments and passes the caller.
    let detour = unsafe { monaka_hook::probe::as_detour::<FramePresentFn>(frame_present_stub) };
    hooks.site(&FRAME_PRESENT_ORIGINAL, renderer, &engine::FRAME_PRESENT, detour)?;
    log!("frame order probe: the scene, prepare and the frame's present request hooked");
    Ok(())
}

/// End of a run: the events, in one go.
pub fn report() {
    if !options().frame_order {
        return;
    }
    let events = EVENTS.locked();
    log!("frame order probe: {} events:\n{}", events.lines.len(), events.lines.join("\n"));
}
