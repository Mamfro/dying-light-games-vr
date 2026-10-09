//! Streamline inputs: the renderer (rd3d12) tags its depth, motion vectors and colours for DLSS
//! each frame (`slSetTagForFrame`) and sends the camera it rendered with (`slSetConstants`), both
//! through its import table from sl.interposer.dll. `dlss_reset=1` passes the constants on with
//! DLSS's reset flag set, so its history never carries one eye into the other. Layouts:
//! [`monaka_streamline`].
//!
//! Its probes (`probe_streamline`, `probe_motion`) are in `research::streamline` and
//! `research::motion`.

use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::log;
use monaka_streamline::{Constants, EvaluateFn, FreeFn, INTERPOSER as STREAMLINE, SetConstantsFn, SetOptionsFn, SetTagFn, constants as at, feature, read_tags};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

static SET_TAG: Original<SetTagFn> = Original::new();
static SET_CONSTANTS: Original<SetConstantsFn> = Original::new();
static TAG_CALLS: AtomicU64 = AtomicU64::new(0);
static CONSTANT_CALLS: AtomicU64 = AtomicU64::new(0);
static RESETS: AtomicU64 = AtomicU64::new(0);

unsafe extern "system" fn set_tag(frame: usize, viewport: *const u8, tags: *const u8, count: u32, commands: *mut c_void) -> i32 {
    let _flight = InFlight::enter();
    let n = TAG_CALLS.fetch_add(1, Relaxed) + 1;
    let all = read_tags(tags, count);
    crate::research::streamline::tagged(n, frame, viewport, &all, commands);
    crate::output::framegen::at_tags(&all);
    if let Some((ours, eye_viewport)) = crate::output::dlss::tags(viewport, tags, count) {
        // SAFETY: the original, with this eye's viewport and tags (the motion vectors ours), which
        // live until the frame's next tags.
        return unsafe { SET_TAG.get()(frame, eye_viewport, ours, count, commands) };
    }
    // SAFETY: the original, with the game's arguments.
    unsafe { SET_TAG.get()(frame, viewport, tags, count, commands) }
}

/// `dlss_reset=1`: DLSS is told to drop its history every frame, so it never blends one eye into
/// the other (at the cost of its temporal anti-aliasing).
static RESET_EVERY_FRAME: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn set_constants(constants: *const u8, frame: usize, viewport: *const u8) -> i32 {
    let _flight = InFlight::enter();
    let n = CONSTANT_CALLS.fetch_add(1, Relaxed) + 1;
    crate::research::streamline::constants(constants);
    // The camera this frame is rendered with labels it (its eye and pose) for the present.
    let eye = (!constants.is_null()).then(|| mem::read::<[f32; 3]>(constants as usize + at::POSITION)).flatten().and_then(crate::view::stereo::label_frame);
    crate::output::framegen::at_constants(constants, eye);
    if !constants.is_null()
        && let Some((ours, eye_viewport)) = crate::output::dlss::constants(constants, viewport, eye)
    {
        // SAFETY: the original, with the game's frame and this eye's constants and viewport, which
        // live until the frame's next constants.
        return unsafe { SET_CONSTANTS.get()(ours, frame, eye_viewport) };
    }
    if RESET_EVERY_FRAME.load(Relaxed)
        && let Some(mut copy) = Constants::read(constants)
    {
        copy.set_reset(true);
        RESETS.fetch_add(1, Relaxed);
        // SAFETY: the original, with the game's frame and viewport and a copy of its constants
        // that lives until it returns.
        return unsafe { SET_CONSTANTS.get()(copy.as_ptr(), frame, viewport) };
    }
    crate::research::streamline::unchanged(n, constants, frame, viewport);
    // SAFETY: the original, with the game's arguments.
    unsafe { SET_CONSTANTS.get()(constants, frame, viewport) }
}

static EVALUATE: Original<EvaluateFn> = Original::new();
static EVALUATE_CALLS: AtomicU64 = AtomicU64::new(0);

/// `slEvaluateFeature`: frame generation's and per-eye DLSS's inputs (and `probe_streamline`'s).
unsafe extern "system" fn evaluate(feature: u32, frame: *const u8, inputs: *const *const u8, count: u32, commands: *mut c_void) -> i32 {
    let _flight = InFlight::enter();
    let n = EVALUATE_CALLS.fetch_add(1, Relaxed) + 1;
    crate::research::streamline::evaluated(n, feature, frame, inputs, count, commands);
    if feature == feature::DLSS && crate::output::framegen::enabled() {
        crate::output::framegen::at_evaluate(native_list(commands));
    }
    if feature == feature::DLSS
        && !inputs.is_null()
        && let Some(ours) = crate::output::dlss::evaluate(inputs, count, native_list(commands))
    {
        // SAFETY: the original, with the game's frame and command list and this eye's viewport (the
        // motion-vector rebase recorded just before on the same list).
        return unsafe { EVALUATE.get()(feature, frame, ours.as_ptr(), count, commands) };
    }
    // SAFETY: the original, with the game's arguments.
    unsafe { EVALUATE.get()(feature, frame, inputs, count, commands) }
}

static GET_NATIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The game's command list behind Streamline's proxy ([`monaka_streamline::native_list`]).
fn native_list(commands: *mut c_void) -> Option<windows::Win32::Graphics::Direct3D12::ID3D12GraphicsCommandList> {
    // SAFETY: GET_NATIVE is Streamline's export (or 0), and `commands` the list the game's
    // Streamline call was given.
    unsafe { monaka_streamline::native_list(GET_NATIVE.load(Relaxed), commands) }
}

static SET_OPTIONS: Original<SetOptionsFn> = Original::new();

/// `slDLSSSetOptions` through the pointer rd3d12 keeps: the game's call, then the same options for
/// both eye viewports.
unsafe extern "system" fn set_options(viewport: *const u8, options: *const u8) -> i32 {
    let _flight = InFlight::enter();
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { SET_OPTIONS.get()(viewport, options) };
    if result == 0 {
        // SAFETY: Streamline's own function, with our eye viewport copies and the game's options.
        crate::output::dlss::on_options(viewport, options, |v, o| unsafe { SET_OPTIONS.get()(v, o) });
    }
    result
}

/// After the hooks are out: Streamline frees the DLSS resources it holds for the eye viewports.
pub fn release() {
    let (Some(viewports), Some(free)) = (crate::output::dlss::eye_viewports(), Module::find(STREAMLINE).and_then(|m| m.export("slFreeResources"))) else { return };
    // SAFETY: Streamline's exported slFreeResources(Feature, const ViewportHandle&).
    let free: FreeFn = unsafe { std::mem::transmute(free) };
    // SAFETY: frees resources for our own viewport copies.
    let results = viewports.map(|v| unsafe { free(feature::DLSS, v.as_ptr()) });
    log!("DLSS resources of the eye viewports freed (results {results:?})");
}

/// Hooks the renderer's Streamline imports (its import table: no code patched):
/// `reset_every_frame` drops DLSS's history every frame, `per_eye` gives each eye its own DLSS
/// viewport, and `framegen` feeds per-eye frame generation its inputs.
///
/// # Safety
/// `renderer` must be the game's D3D12 renderer DLL.
pub unsafe fn install(hooks: &mut Hooks, renderer: &Module, reset_every_frame: bool, per_eye: bool, framegen: bool) {
    RESET_EVERY_FRAME.store(reset_every_frame, Relaxed);
    if reset_every_frame {
        log!("DLSS history reset every frame (dlss_reset=1)");
    }
    let Some(interposer) = monaka_streamline::interposer() else { return };
    if framegen {
        // Frame generation records its motion-vector rebase on the game's native list at DLSS's
        // evaluate, as per-eye DLSS does.
        match interposer.export("slGetNativeInterface") {
            Some(native) => GET_NATIVE.store(native, Relaxed),
            None => log!("frame generation: {STREAMLINE} does not export slGetNativeInterface; no inputs"),
        }
    }
    if per_eye {
        // The options pointer rd3d12 keeps (it is filled the first time DLSS runs).
        let slot = renderer.at(crate::engine::DLSS_SET_OPTIONS_POINTER);
        let native = interposer.export("slGetNativeInterface");
        // SAFETY: the slot holds Streamline's slDLSSSetOptions(const ViewportHandle&, const DLSSOptions&).
        match (native, unsafe { hooks.pointer(&SET_OPTIONS, "slDLSSSetOptions", slot, set_options as SetOptionsFn) }) {
            (Some(native), Ok(())) => {
                GET_NATIVE.store(native, Relaxed);
                crate::output::dlss::ENABLED.store(true, Relaxed);
                log!("per-eye DLSS armed: it starts once the game sets its DLSS options");
            }
            (None, _) => log!("per-eye DLSS off: {STREAMLINE} does not export slGetNativeInterface"),
            (_, Err(e)) => log!("per-eye DLSS off: {e}"),
        }
    }
    // SAFETY: the caller's guarantee: the renderer imports these from the interposer, with these
    // signatures (sl_core_api.h).
    unsafe {
        monaka_streamline::hook_inputs(hooks, renderer, (&SET_TAG, set_tag as SetTagFn), (&SET_CONSTANTS, set_constants as SetConstantsFn));
        if (crate::research::options().streamline || per_eye || framegen) && let Err(e) = hooks.import(&EVALUATE, "slEvaluateFeature", renderer, STREAMLINE, "slEvaluateFeature", evaluate as EvaluateFn) {
            log!("Streamline evaluate not hooked: {e}");
        }
    }
}

pub fn report(seconds: f64) {
    static LAST: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    let now = [TAG_CALLS.load(Relaxed), CONSTANT_CALLS.load(Relaxed)];
    let rate = monaka_producer::rates(now, &LAST, seconds);
    log!("streamline: tag calls {:.1}/s, constants {:.1}/s, DLSS resets {}; {}", rate[0], rate[1], RESETS.load(Relaxed), crate::output::dlss::report());
}
