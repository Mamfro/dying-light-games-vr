//! Provider: a frame-input source (`FrameInputs`, from the game's Streamline calls). It also holds
//! DLSS frame generation off in same-frame stereo, hands depth stereo its copies
//! (`crate::extras::depth_stereo`), per-eye frame generation the game's UI layer
//! (`monaka_stereo::framegen12::pairs`) and the world markers (`crate::extras::markers`) their
//! camera.
//!
//! Streamline inputs. For DLSS the game tags its depth, motion vectors and colours each frame
//! (`slSetTagForFrame`) and sends the camera it rendered with (`slSetConstants`), both imported
//! from sl.interposer.dll by the renderer DLL, so their import slots are hooked there
//! (`monaka_streamline::imports`, with this game's handlers). Depth stereo copies the depth and
//! the UI layer and reads the projection from them.
//!
//! DLSS frame generation interpolates between consecutive presented frames. In same-frame stereo
//! those alternate between the eyes, so generation is held off there (off once for the viewport,
//! and the game's later options rewritten). Depth stereo presents one centre view per frame, so
//! generation may run; the game tags its UI layer only while it does, and depth stereo needs it.
//!
//! Layouts: [`monaka_streamline`]; DLSSGOptions (120 bytes): mode +32.

use monaka_hook::AtomicF32;
use crate::config;
use crate::extras::depth_stereo;
use crate::providers::{eye_site, hud};
use monaka_stereo::framegen12::pairs as framegen;
use monaka_stereo::same_frame::PAIRS;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::log;
use monaka_streamline::imports::{Handlers, Install};
use monaka_streamline::{Constants, GetFeatureFunctionFn, Tag, constants as at, SetOptionsFn, buffer, feature, viewport_id};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use windows::Win32::Graphics::Direct3D12::ID3D12GraphicsCommandList;
use windows::core::Interface;
use monaka_core::sync::Locked;

static SET_OPTIONS: Original<SetOptionsFn> = Original::new();

/// The camera constants depth stereo needs: P00, the near plane, and reverse-Z.
pub static PROJECTION_SCALE: AtomicF32 = AtomicF32::zero();
pub static NEAR_PLANE: AtomicF32 = AtomicF32::zero();
pub static DEPTH_INVERTED: AtomicBool = AtomicBool::new(false);
/// Tags seen per buffer type (0..31) and render eye (0 outside a pair, 1, 2).
static TAG_TYPES: [[AtomicU64; 32]; 3] = [const { [const { AtomicU64::new(0) }; 32] }; 3];
static GENERATION_HELD: AtomicU64 = AtomicU64::new(0);
/// Viewports generation was held off for; per-eye DLSS gives each eye its own id.
static GENERATION_OFF: Mutex<Vec<u32>> = Mutex::new(Vec::new());

const DLSSG_OFF: u32 = 0;

/// `sl::DLSSGOptions` with its defaults (version 5).
#[repr(C, align(8))]
struct DlssgOptions([u8; 120]);

impl DlssgOptions {
    fn off() -> Self {
        const GUID: [u8; 16] = [0xcb, 0xf1, 0xc5, 0xfa, 0xfd, 0x2d, 0x36, 0x4f, 0xa1, 0xe6, 0x3a, 0x9e, 0x86, 0x52, 0x56, 0xc5];
        let mut bytes = [0u8; 120];
        bytes[8..24].copy_from_slice(&GUID);
        bytes[24..32].copy_from_slice(&5u64.to_le_bytes());
        bytes[32..36].copy_from_slice(&DLSSG_OFF.to_le_bytes());
        // numFramesToGenerate = 1; bReserved15 = eInvalid.
        bytes[36..40].copy_from_slice(&1u32.to_le_bytes());
        bytes[104] = 2;
        Self(bytes)
    }
}

/// Same-frame stereo holds generation off while VR runs; depth stereo lets it run.
fn hold_generation() -> bool {
    !config::get().depth_stereo && PAIRS.running()
}

unsafe extern "system" fn set_options(viewport: *const u8, options: *const u8) -> i32 {
    let _guard = InFlight::enter();
    let mode = mem::read::<u32>(options as usize + 32).unwrap_or(DLSSG_OFF);
    if hold_generation() && mode != DLSSG_OFF {
        let mut off = [0u8; 120];
        if mem::read_bytes(options as usize, &mut off) {
            off[32..36].copy_from_slice(&DLSSG_OFF.to_le_bytes());
            let off = DlssgOptions(off);
            GENERATION_HELD.fetch_add(1, Ordering::Relaxed);
            // SAFETY: the original, with the game's viewport and a copy of its options, mode off.
            return unsafe { SET_OPTIONS.get()(viewport, off.0.as_ptr()) };
        }
    }
    // SAFETY: the original, with the game's arguments.
    unsafe { SET_OPTIONS.get()(viewport, options) }
}

fn hold_generation_off(viewport: *const u8) {
    let id = viewport_id(viewport).unwrap_or(u32::MAX);
    if !SET_OPTIONS.is_set() {
        return;
    }
    {
        let mut done = GENERATION_OFF.locked();
        if done.contains(&id) || done.len() >= 16 {
            return;
        }
        done.push(id);
    }
    let off = DlssgOptions::off();
    // SAFETY: Streamline's own options function, with the game's viewport and default options.
    let result = unsafe { SET_OPTIONS.get()(viewport, off.0.as_ptr()) };
    GENERATION_HELD.fetch_add(1, Ordering::Relaxed);
    log!("frame generation held off for viewport {id} (result {result}); re-enable it in the game's options after VR if wanted");
}

/// `slSetTagForFrame` (call `n`): the tags logged and counted, frame generation held off, and the
/// buffers depth stereo needs copied on the game's list, frame generation's UI layer kept. Each
/// eye's depth and motion vectors are the frame inputs' (`providers::stereo`).
fn tagged(n: u64, _: usize, viewport: *const u8, all: &[Tag], commands: *mut c_void) {
    if n <= 4 {
        for tag in all {
            log!("sl tag call {n} viewport={:?} type={} native={:#x} state={} resource type={}", viewport_id(viewport), tag.kind, tag.native, tag.state, tag.resource_type);
        }
    }
    let eye = eye_site::RENDER_EYE.load(Ordering::Relaxed).min(2) as usize;
    for tag in all {
        if let Some(count) = TAG_TYPES[eye].get(tag.kind as usize) {
            let n = count.fetch_add(1, Ordering::Relaxed);
            if n < 2
                && let Some(resource) = tag.texture()
            {
                // SAFETY: reads a descriptor.
                let d = unsafe { resource.GetDesc() };
                log!("sl tag type {} (render eye {eye}): {}x{} format {} state {} commands {}", tag.kind, d.Width, d.Height, d.Format.0, tag.state, !commands.is_null());
            }
        }
    }
    if hold_generation() {
        hold_generation_off(viewport);
    }
    let config = config::get();
    if all.iter().any(|t| t.kind == buffer::UI_COLOR_AND_ALPHA) {
        hud::ui_tagged();
    }
    let list = (!commands.is_null()).then_some(commands);
    if let Some(list) = list
        && (config.depth_stereo || config.has(config::debug::HUDLESS_LEFT) || framegen::enabled())
    {
        // SAFETY: the game's command list, being recorded for this frame.
        let list = unsafe { ID3D12GraphicsCommandList::from_raw_borrowed(&list) };
        for tag in all {
            let (Some(list), Some(resource)) = (list, tag.texture()) else { continue };
            let state = tag.state();
            match tag.kind {
                buffer::DEPTH if config.depth_stereo => depth_stereo::copy_depth(list, resource, state),
                buffer::UI_COLOR_AND_ALPHA if framegen::enabled() => tagged_ui(resource, state),
                buffer::UI_COLOR_AND_ALPHA if config.depth_stereo || config.has(config::debug::HUDLESS_LEFT) => depth_stereo::copy_ui(list, resource, state),
                _ => {}
            }
        }
    }
}

/// The game tagged the UI layer of the eye rendering now: to per-eye frame generation, which keeps
/// the HUD out of its interpolation with it. With the HUD kept out of the frame by
/// `eng_chr::providers::hudlayer`, the game's layer is empty and is not given.
fn tagged_ui(source: &windows::Win32::Graphics::Direct3D12::ID3D12Resource, state: windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATES) {
    if eng_chr::providers::hudlayer::redirecting() {
        return;
    }
    if let Some((eye, _)) = eye_site::render_frame() {
        framegen::tagged_ui(eye, source, state);
    }
}

/// `slSetConstants`: the projection depth stereo needs and the HUD markers' camera (each eye's
/// camera is the frame inputs'). No eye label: per-eye DLSS here is the renderer's own
/// ([`dlss`](crate::providers::dlss)).
fn constants(_: u64, constants: *const u8, _: usize, _: *const u8) -> Option<usize> {
    let read = Constants::read(constants);
    if let Some(c) = &read {
        let (p00, near, inverted) = (c.f32(at::CAMERA_VIEW_TO_CLIP), c.f32(at::NEAR), c.byte(at::DEPTH_INVERTED));
        monaka_producer::log_first!(4, "sl constants: P00={p00:.4} near={near:.4} depthInverted={inverted}");
        PROJECTION_SCALE.store(p00);
        NEAR_PLANE.store(near);
        crate::extras::markers::set_camera(c);
        DEPTH_INVERTED.store(inverted == 1, Ordering::Release);
    }
    if framegen::enabled()
        && let Some(c) = &read
    {
        let camera = monaka_framegen::camera(c);
        monaka_producer::log_first!(
            6,
            "sl camera (render eye {}): jitter {:?} mvec scale {:?} near {} far {} fov {} pos {:?} fwd {:?}",
            eye_site::RENDER_EYE.load(Ordering::Relaxed),
            camera.jitter,
            camera.motion_vector_scale,
            camera.near,
            camera.far,
            camera.fov_vertical,
            camera.position,
            camera.forward
        );
    }
    None
}

/// Prepares the Streamline hooks: the renderer's imports, and the frame-generation options function
/// as the game obtains it. Missing pieces are logged, not fatal.
///
/// # Safety
/// `renderer` must be the game's renderer DLL.
pub unsafe fn install(hooks: &mut Hooks, renderer: &monaka_hook::module::Module) {
    let handlers = Handlers { tags: Some(tagged), constants: Some(constants), ..Handlers::default() };
    // SAFETY: the caller's guarantee: the renderer imports both from the interposer.
    let Some(interposer) = (unsafe { monaka_streamline::imports::install(hooks, renderer, Install { handlers, ..Install::default() }) }) else { return };
    let Some(get_feature_function) = interposer.export("slGetFeatureFunction") else {
        log!("frame generation options not reachable; keep Frame Generation off in the game for VR");
        return;
    };
    // SAFETY: `slGetFeatureFunction(Feature, const char*, void*&)`.
    let get_feature_function: GetFeatureFunctionFn = unsafe { std::mem::transmute(get_feature_function) };
    let mut function = std::ptr::null_mut();
    // SAFETY: asks Streamline for its own function into a local.
    let result = unsafe { get_feature_function(feature::DLSS_G, c"slDLSSGSetOptions".as_ptr() as *const u8, &mut function) };
    if result != 0 || function.is_null() {
        log!("frame generation options not reachable (result {result}); keep Frame Generation off in the game for VR");
        return;
    }
    // Already hooked by someone else (a jump first): their hook stays in the chain, after ours.
    // SAFETY: `slDLSSGSetOptions(const ViewportHandle&, const DLSSGOptions&)`.
    if let Err(e) = unsafe { hooks.inline_chained(&SET_OPTIONS, "slDLSSGSetOptions", function as usize, set_options as SetOptionsFn) } {
        log!("frame generation options not hooked: {e}; keep Frame Generation off in the game for VR");
    }
}

pub fn report() -> String {
    let mut types = String::new();
    for (eye, counts) in TAG_TYPES.iter().enumerate() {
        let seen: Vec<String> = counts.iter().enumerate().filter(|(_, c)| c.load(Ordering::Relaxed) > 0).map(|(t, c)| format!("{t}:{}", c.load(Ordering::Relaxed))).collect();
        if !seen.is_empty() {
            types += &format!(" eye {eye} [{}]", seen.join(" "));
        }
    }
    format!("Streamline tag calls={} (types by eye:{types}), frame generation held off={}", monaka_streamline::imports::tag_calls(), GENERATION_HELD.load(Ordering::Relaxed))
}
