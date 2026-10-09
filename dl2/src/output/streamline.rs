//! Streamline inputs. For DLSS the game tags its depth, motion vectors and colours each frame
//! (`slSetTagForFrame`) and sends the camera it rendered with (`slSetConstants`), both imported
//! from sl.interposer.dll by the renderer DLL, so their import slots are hooked there. Depth stereo
//! copies the depth and the UI layer and reads the projection from them.
//!
//! DLSS frame generation interpolates between consecutive presented frames. In same-frame stereo
//! those alternate between the eyes, so generation is held off there (off once for the viewport,
//! and the game's later options rewritten). Depth stereo presents one centre view per frame, so
//! generation may run; the game tags its UI layer only while it does, and depth stereo needs it.
//!
//! Layouts: [`monaka_streamline`]; DLSSGOptions (120 bytes): mode +32.

use monaka_hook::AtomicF32;
use crate::{config, output::render12, view::scene};
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::log;
use monaka_streamline::{Constants, GetFeatureFunctionFn, SetConstantsFn, constants as at, SetOptionsFn, SetTagFn, buffer, feature, read_tags, viewport_id};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use windows::Win32::Graphics::Direct3D12::ID3D12GraphicsCommandList;
use windows::core::Interface;

static SET_TAG: Original<SetTagFn> = Original::new();
static SET_CONSTANTS: Original<SetConstantsFn> = Original::new();
static SET_OPTIONS: Original<SetOptionsFn> = Original::new();

/// The camera constants depth stereo needs: P00, the near plane, and reverse-Z.
pub static PROJECTION_SCALE: AtomicF32 = AtomicF32::zero();
pub static NEAR_PLANE: AtomicF32 = AtomicF32::zero();
pub static DEPTH_INVERTED: AtomicBool = AtomicBool::new(false);
static TAG_CALLS: AtomicU64 = AtomicU64::new(0);
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
    !config::get().depth_stereo && scene::vr_active()
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
        let mut done = GENERATION_OFF.lock().unwrap_or_else(|e| e.into_inner());
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

unsafe extern "system" fn set_tag(frame: usize, viewport: *const u8, tags: *const u8, count: u32, commands: *mut c_void) -> i32 {
    let _guard = InFlight::enter();
    let n = TAG_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    let all = read_tags(tags, count);
    if n <= 4 {
        for tag in &all {
            log!("sl tag call {n} viewport={:?} type={} native={:#x} state={} resource type={}", viewport_id(viewport), tag.kind, tag.native, tag.state, tag.resource_type);
        }
    }
    let eye = crate::view::scene::RENDER_EYE.load(Ordering::Relaxed).min(2) as usize;
    for tag in &all {
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
    // The frame-generation call (it carries the UI layer or the HUD-less frame): its buffers are
    // "valid until present", complete only by then.
    let generation_call = all.iter().any(|t| t.kind == buffer::UI_COLOR_AND_ALPHA || t.kind == buffer::HUDLESS_COLOR);
    if all.iter().any(|t| t.kind == buffer::UI_COLOR_AND_ALPHA) {
        render12::ui_tagged();
    }
    let list = (!commands.is_null()).then_some(commands);
    if let Some(list) = list
        && (config.depth_stereo || config.has(config::debug::HUDLESS_LEFT) || crate::output::framegen::enabled())
    {
        // SAFETY: the game's command list, being recorded for this frame.
        let list = unsafe { ID3D12GraphicsCommandList::from_raw_borrowed(&list) };
        for tag in &all {
            let (Some(list), Some(resource)) = (list, tag.texture()) else { continue };
            let state = tag.state();
            match tag.kind {
                buffer::DEPTH if config.depth_stereo => render12::copy_depth(list, resource, state),
                buffer::DEPTH if crate::output::framegen::enabled() => crate::output::framegen::copy_input(list, crate::output::framegen::Input::Depth, resource, state, generation_call),
                buffer::MOTION_VECTORS if crate::output::framegen::enabled() => {
                    crate::output::framegen::copy_input(list, crate::output::framegen::Input::MotionVectors, resource, state, generation_call)
                }
                buffer::UI_COLOR_AND_ALPHA if crate::output::framegen::enabled() => crate::output::framegen::copy_input(list, crate::output::framegen::Input::Ui, resource, state, true),
                buffer::UI_COLOR_AND_ALPHA if config.depth_stereo || config.has(config::debug::HUDLESS_LEFT) => render12::copy_ui(list, resource, state),
                _ => {}
            }
        }
    }
    // SAFETY: the original, with the game's arguments.
    unsafe { SET_TAG.get()(frame, viewport, tags, count, commands) }
}

unsafe extern "system" fn set_constants(constants: *const u8, frame: usize, viewport: *const u8) -> i32 {
    let _guard = InFlight::enter();
    let read = Constants::read(constants);
    if let Some(c) = &read {
        let (p00, near, inverted) = (c.f32(at::CAMERA_VIEW_TO_CLIP), c.f32(at::NEAR), c.byte(at::DEPTH_INVERTED));
        monaka_producer::log_first!(4, "sl constants: P00={p00:.4} near={near:.4} depthInverted={inverted}");
        PROJECTION_SCALE.store(p00);
        NEAR_PLANE.store(near);
        crate::hud::markers::set_camera(c);
        DEPTH_INVERTED.store(inverted == 1, Ordering::Release);
    }
    if crate::output::framegen::enabled()
        && let Some(c) = &read
    {
        let camera = monaka_framegen::camera(c);
        monaka_producer::log_first!(
            6,
            "sl camera (render eye {}): jitter {:?} mvec scale {:?} near {} far {} fov {} pos {:?} fwd {:?}",
            crate::view::scene::RENDER_EYE.load(Ordering::Relaxed),
            camera.jitter,
            camera.motion_vector_scale,
            camera.near,
            camera.far,
            camera.fov_vertical,
            camera.position,
            camera.forward
        );
        crate::output::framegen::set_camera(camera);
    }
    // SAFETY: the original, with the game's arguments.
    unsafe { SET_CONSTANTS.get()(constants, frame, viewport) }
}

/// Prepares the Streamline hooks: the renderer's imports, and the frame-generation options function
/// as the game obtains it. Missing pieces are logged, not fatal.
///
/// # Safety
/// `renderer` must be the game's renderer DLL.
pub unsafe fn install(hooks: &mut Hooks, renderer: &monaka_hook::module::Module) {
    let Some(interposer) = monaka_streamline::interposer() else { return };
    // SAFETY: the caller's guarantee: the renderer imports both from the interposer.
    unsafe { monaka_streamline::hook_inputs(hooks, renderer, (&SET_TAG, set_tag as SetTagFn), (&SET_CONSTANTS, set_constants as SetConstantsFn)) };
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
    format!("Streamline tag calls={} (types by eye:{types}), frame generation held off={}", TAG_CALLS.load(Ordering::Relaxed), GENERATION_HELD.load(Ordering::Relaxed))
}
