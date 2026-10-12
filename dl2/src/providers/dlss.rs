//! Provider: where Dying Light 2's renderer calls DLSS itself and where its scene keeps the camera
//! history, for the per-eye upscaler through a renderer's own DLSS calls
//! (`monaka_stereo::upscaler::own_dlss`): the calls hooked ([`install`]), the constants packet's
//! layout ([`PACKET`]), the scene's seven camera writers (`engine::CAMERA_WRITER_RETURNS`), the
//! history entries' and the external pass's sizes. The eye site marks each scene call with its eye
//! ([`HISTORY`]'s `begin_scene` and `end_scene`).
//!
//! On D3D11 DLSS runs inside the pair's queue drain, on the pair's thread (its eye is the one being
//! dispatched); on D3D12 at the eye the scene draws.
//! Adapted from farmerarmor/DyingLight2VR TemporalHistory.h and DlssHistory.h (MIT).

use crate::config::{self, debug};
use crate::providers::engine::{self, CAMERA_WRITER_RETURNS, EXTERNAL_PASS, ExternalPassFn, HISTORY_LOOKUP, HistoryLookupFn, Packet12Fn, PacketFn, Target};
use crate::providers::eye_site::{RENDER_EYE, local};
use crate::providers::game;
use monaka_core::sync::Locked;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::Rejection;
use monaka_stereo::upscaler::own_dlss::{self, EyeCameras, PacketLayout, all_writers, writer_bit};
use std::sync::Mutex;
use std::sync::atomic::Ordering;

/// The DLSS constants packet as the renderer keeps it: its reset flag, render size, projection
/// (column-major), camera position and right, up and view-z rows, and the clip-to-previous-clip
/// matrices.
pub const PACKET: PacketLayout =
    PacketLayout { bytes: 0x1b0, reset: 0x18, size: [0x24, 0x28], projection: 0x30, position: 0x170, basis: [0x188, 0x17c, 0x194], to_previous: 0xf0, to_current: 0x130 };

/// Each eye's previous camera: the history's entries (0x140 bytes) and the external pass's block
/// (0x90 bytes at +0x20).
pub static HISTORY: EyeCameras<0x140, 0x90> = EyeCameras::new();
/// Where the external pass keeps its previous camera.
const EXTERNAL_BLOCK: usize = 0x20;

static HISTORY_LOOKUP_ORIGINAL: Original<HistoryLookupFn> = Original::new();
static EXTERNAL_PASS_ORIGINAL: Original<ExternalPassFn> = Original::new();
static DLSS_EVALUATE_11: Original<PacketFn> = Original::new();
static DLSS_CONSTANTS_11: Original<PacketFn> = Original::new();
static DLSS_EVALUATE_12: Original<Packet12Fn> = Original::new();
static DLSS_CONSTANTS_12: Original<Packet12Fn> = Original::new();
static DLSS_LOCK: Mutex<()> = Mutex::new(());

/// Every one of the scene's camera writers' bits.
pub fn all_camera_writers() -> u32 {
    all_writers(CAMERA_WRITER_RETURNS.len())
}

/// The history's epoch now.
pub fn epoch() -> u32 {
    HISTORY.epoch()
}

/// A scene call starts (`eye` 1 or 2; 0 the game's own history) in `game`'s frame, keyed `token`, on
/// `camera`. Its eye.
pub fn begin_scene(game: usize, token: u32, eye: u32, camera: usize) -> u32 {
    let counter = if eye != 0 { mem::read::<u32>(game + engine::GAME_COUNTER).unwrap_or(0) } else { 0 };
    HISTORY.begin_scene(counter, token, eye, camera)
}

/// The scene call is over: the camera it wrote banked as this eye's previous camera.
pub fn end_scene(eye: u32, token: u32) {
    // SAFETY: the original lookup on the tree the scene wrote, with this scene's key.
    HISTORY.end_scene(eye, token, all_camera_writers(), |tree, key| unsafe { HISTORY_LOOKUP_ORIGINAL.get()(tree, key) });
}

monaka_hook::caller_shim!(history_lookup_stub => history_lookup);

unsafe extern "system" fn history_lookup(tree: usize, key: *const u32, _: usize, caller: usize) -> usize {
    let _guard = InFlight::enter();
    // SAFETY: the original, with the game's arguments.
    let original = unsafe { HISTORY_LOOKUP_ORIGINAL.get()(tree, key) };
    // SAFETY: the game passes a valid key pointer.
    let key = unsafe { *key };
    let writer = writer_bit(&CAMERA_WRITER_RETURNS, caller.wrapping_sub(game::get().engine));
    // SAFETY: the lookup returns a camera history entry of 0x140 bytes (or 0).
    unsafe { HISTORY.lookup(tree, key, original, writer) }
}

/// This pass reads and writes only its previous-camera block at +0x20..+0xaf.
unsafe extern "system" fn external_pass(object: usize, context: usize) -> usize {
    let _guard = InFlight::enter();
    // SAFETY: the pass object holds 0x90 bytes of previous camera at +0x20, used by the pass alone;
    // the original runs with the game's arguments.
    unsafe { HISTORY.around_pass(object, EXTERNAL_BLOCK, || EXTERNAL_PASS_ORIGINAL.get()(object, context)) }
}

fn signature(what: impl std::fmt::Display) -> Rejection {
    Rejection::revision(format!("signature mismatch: {what}"))
}

/// Checks the camera writers' calls and hooks the history's lookups and the renderer's DLSS calls
/// (`renderer`'s, D3D11's when `d3d11`).
///
/// # Safety
/// `engine` must be the inspected engine build (`ENGINE_SHA256`), `renderer` the renderer it loaded.
pub unsafe fn install(hooks: &mut Hooks, engine: &Module, renderer: &Module, d3d11: bool) -> Result<(), Rejection> {
    HISTORY.set_native(config::debug(debug::NATIVE_CAMERA_HISTORY));
    for &rva in &CAMERA_WRITER_RETURNS {
        let call = engine.at(rva - 5);
        let target = mem::read::<i32>(call + 1).map(|rel| (call as i64 + 5 + rel as i64) as usize);
        if mem::read::<u8>(call) != Some(0xe8) || target != Some(engine.at(HISTORY_LOOKUP.0)) {
            return Err(signature(format_args!("camera history writer {rva:#x}")));
        }
    }
    // SAFETY: every target is checked against its exact prologue; every detour (or shim stub) has
    // the target's type.
    unsafe {
        let at = |target: &Target| engine.at(target.0);
        hooks.inline(&HISTORY_LOOKUP_ORIGINAL, "camera history", at(&HISTORY_LOOKUP), HISTORY_LOOKUP.1, monaka_hook::probe::as_detour::<HistoryLookupFn>(history_lookup_stub))?;
        hooks.inline(&EXTERNAL_PASS_ORIGINAL, "external history pass", at(&EXTERNAL_PASS), EXTERNAL_PASS.1, external_pass as ExternalPassFn)?;
    }
    if d3d11 {
        hooks.site(&DLSS_EVALUATE_11, renderer, &engine::DLSS_EVALUATE_11, dlss_evaluate_11)?;
        hooks.site(&DLSS_CONSTANTS_11, renderer, &engine::DLSS_CONSTANTS_11, dlss_constants_11)?;
    } else {
        hooks.site(&DLSS_EVALUATE_12, renderer, &engine::DLSS_EVALUATE_12, dlss_evaluate_12)?;
        hooks.site(&DLSS_CONSTANTS_12, renderer, &engine::DLSS_CONSTANTS_12, dlss_constants_12)?;
    }
    Ok(())
}

/// D3D11: DLSS runs inside the drain on the pair's thread; each eye gets its own feature and matrices.
unsafe fn scoped_11(packet: usize, original: PacketFn, evaluate: bool) -> usize {
    let _guard = InFlight::enter();
    let dispatch = local(|l| l.dispatch_eye.get());
    let eye = if config::debug(debug::NATIVE_DLSS_HISTORY) || dispatch > 2 { 0 } else { dispatch };
    let Some(r11) = game::get().renderer11.filter(|_| eye != 0) else {
        // SAFETY: the original, with the renderer's argument.
        return unsafe { original(packet) };
    };
    // SAFETY: the renderer's DLSS viewport id (a u32), put back after the call; the packet is the
    // renderer's constants packet; DLSS runs on the pair's thread alone.
    unsafe { own_dlss::for_eye(&PACKET, eye, (r11 + engine::DLSS_VIEWPORT_11) as *mut u32, None, packet, evaluate, epoch(), |packet| original(packet)) }
}

unsafe extern "system" fn dlss_evaluate_11(packet: usize) -> usize {
    // SAFETY: forwards the renderer's argument.
    unsafe { scoped_11(packet, DLSS_EVALUATE_11.get(), true) }
}

unsafe extern "system" fn dlss_constants_11(packet: usize) -> usize {
    // SAFETY: forwards the renderer's argument.
    unsafe { scoped_11(packet, DLSS_CONSTANTS_11.get(), false) }
}

/// D3D12 DLSS takes (native state, packet). The viewport id lives at (*rd3d12+DLSS_OWNER)+0x50,
/// falling back to the cached copy at +0x30 of the constants state or of the global DLSS state.
unsafe fn scoped_12(state: usize, packet: usize, original: Packet12Fn, evaluate: bool) -> usize {
    let _guard = InFlight::enter();
    let eye = if config::debug(debug::NATIVE_DLSS_HISTORY) { 0 } else { RENDER_EYE.load(Ordering::Acquire) };
    let r12 = game::get().renderer12.unwrap_or(0);
    let owner = mem::read::<usize>(r12 + engine::DLSS_OWNER_12).unwrap_or(0);
    let cache = if evaluate { mem::read::<usize>(r12 + engine::DLSS_GLOBAL_12).unwrap_or(0) } else { state };
    let viewport = if owner != 0 {
        owner + 0x50
    } else if cache != 0 {
        cache + 0x30
    } else {
        0
    };
    if !(1..=2).contains(&eye) || viewport == 0 {
        // SAFETY: the original, with the renderer's arguments.
        return unsafe { original(state, packet) };
    }
    let _lock = DLSS_LOCK.locked();
    let cache = (cache != 0).then(|| (cache + 0x30) as *mut u32);
    // SAFETY: the renderer's DLSS state holds the viewport id (and its cached copy) as u32s; the
    // ids are put back after the call, under the lock.
    unsafe { own_dlss::for_eye(&PACKET, eye, viewport as *mut u32, cache, packet, evaluate, epoch(), |packet| original(state, packet)) }
}

unsafe extern "system" fn dlss_evaluate_12(state: usize, packet: usize) -> usize {
    // SAFETY: forwards the renderer's arguments.
    unsafe { scoped_12(state, packet, DLSS_EVALUATE_12.get(), true) }
}

unsafe extern "system" fn dlss_constants_12(state: usize, packet: usize) -> usize {
    // SAFETY: forwards the renderer's arguments.
    unsafe { scoped_12(state, packet, DLSS_CONSTANTS_12.get(), false) }
}
