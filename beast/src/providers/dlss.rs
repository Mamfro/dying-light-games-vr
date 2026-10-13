//! Provider: a frame-input source (`FrameInputs`, DLSS's), with the renderer's jitter kept per eye
//! for the per-eye upscaler (`monaka_stereo::upscaler::per_eye`, its component [`crate::PER_EYE`]).
//!
//! DLSS's inputs: the renderer's Streamline imports (`monaka_streamline::imports`), where DLSS's
//! camera constants label each frame with its eye and the frame inputs are collected for the
//! features that ask (`crate::providers::stereo`), and the game's upscaler kept per eye
//! ([`crate::PER_EYE`]: DLSS by `monaka_streamline::per_eye`, FSR by
//! `monaka_framegen::upscale_per_eye` through `crate::providers::fsr`, with the renderer's jitter
//! per eye here).

use crate::providers::engine;
use crate::{Beast, research, stereo_on};
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::game::{Lack, On, OrMissing, Run, Start};
use monaka_streamline::constants as at;
use monaka_streamline::imports::{self, Handlers, Install, PerEye};
use std::sync::atomic::{AtomicBool, Ordering};

/// The upscaler per eye's install (`crate::PER_EYE`): with `dlss_jitter_per_eye`, the renderer's
/// jitter walked per eye ([`frame_jitter`]).
pub(crate) fn install_per_eye(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    start.build.renderer_12.as_ref().or_missing("the game is not on D3D12")?;
    if start.options.switch("dlss_jitter_per_eye", true) {
        start.hooks.site(&FRAME_JITTER, &start.build.renderer, &engine::FRAME_JITTER, frame_jitter)?;
        JITTER_PER_EYE.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// Whether the Streamline imports are hooked (`crate::STREAMLINE`): with stereo, the Streamline
/// probe or `dlss_reset`.
pub(crate) fn streamline_on(run: &Run<'_, Beast>) -> On {
    let wanted = stereo_on(run.options) || research::options().streamline || run.options.switch("dlss_reset", false);
    On::when(wanted, "no DLSS inputs needed")
}

/// The Streamline imports' install (`crate::STREAMLINE`): DLSS's camera constants label each
/// frame with its eye ([`constants`]); per-eye DLSS set up when [`crate::PER_EYE`] is on.
pub(crate) fn install_streamline(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let r12 = start.build.renderer_12.as_ref().or_missing("the game is not on D3D12")?;
    let options = start.options;
    let per_eye = start.is_on(&crate::PER_EYE);
    let install = Install {
        handlers: Handlers {
            tags: Some(research::streamline::tagged),
            constants: Some(constants),
            unchanged: Some(research::streamline::unchanged),
            evaluate: Some(research::streamline::evaluated),
        },
        reset_every_frame: options.switch("dlss_reset", false),
        per_eye: per_eye.then_some(PerEye { options_slot: engine::DLSS_SET_OPTIONS_POINTER, rebase: monaka_framegen::motion::rebase, fix_motion: options.switch("dlss_fix_motion", true) }),
    };
    // Each eye's scene renders against its own previous frame (`crate::providers::eye_history`),
    // unless it is off: then the rebase, and this experiment, how far object motion is carried onto
    // it (two game frames back with alternate eyes, one with same-frame).
    let same_frame = crate::config::method(options) == monaka_core::rendering::StereoMethod::SameFrame;
    let frames_back = if same_frame { 1.0 } else { 2.0 };
    monaka_streamline::per_eye::use_game_history(per_eye && crate::providers::eye_history::wanted(options));
    monaka_streamline::per_eye::set_object_scale(options.number("dlss_object_scale", 0.0, 4.0, frames_back) as f32);
    // SAFETY: rd3d12 is the game's D3D12 renderer DLL.
    unsafe { imports::install(start.hooks, r12, install) };
    research::motion::arm();
    Ok(())
}

/// `slSetConstants`: the camera this frame is rendered with labels it (its eye and pose) for the
/// present and the frame inputs.
fn constants(_: u64, constants: *const u8, _: usize, _: *const u8) -> Option<usize> {
    research::streamline::constants(constants);
    (!constants.is_null()).then(|| mem::read::<[f32; 3]>(constants as usize + at::POSITION)).flatten().and_then(|at| monaka_stereo::feature::DRIVER.label_frame(at))
}

/// The jitter sequence per eye: the game takes frame `n`'s sub-pixel jitter from a sequence whose
/// steps alternate sides (x positive on even steps, negative on odd), so with the eyes alternating
/// each eye's own upscaler history (DLSS's or FSR's: both packets take the jitter from here) would
/// only ever sample half of every pixel. Frame `n` is given step `n / 2` instead: both frames of a
/// pair share a step, and each eye walks the whole sequence.
static FRAME_JITTER: Original<engine::FrameJitterFn> = Original::new();
static JITTER_PER_EYE: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn frame_jitter(out: *mut [f32; 2], frame: u64) -> *mut [f32; 2] {
    let _flight = InFlight::enter();
    let frame = if JITTER_PER_EYE.load(Ordering::Relaxed) && monaka_stereo::upscaler::running() { frame / 2 } else { frame };
    // SAFETY: the original, with the game's output pointer and the step chosen.
    unsafe { FRAME_JITTER.get()(out, frame) }
}
