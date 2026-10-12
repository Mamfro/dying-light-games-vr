//! DLSS's inputs: the renderer's Streamline imports ([`streamline`]), where DLSS's camera constants
//! label each frame with its eye and frame generation takes its inputs, and the per-eye DLSS
//! experiments ([`per_eye`]).

pub(crate) mod per_eye;
pub(crate) mod streamline;

use crate::{Beast, engine, research, stereo, stereo_on};
use monaka_producer::game::{Component, Lack, On, OrMissing, Run, Start};
use std::sync::atomic::Ordering;

/// The renderer's Streamline imports: with stereo, DLSS's camera constants label each frame with
/// its eye; frame generation takes its inputs there. Experiments: `dlss_reset=1` drops DLSS's
/// history every frame, `dlss_per_eye=1` gives each eye its own DLSS viewport
/// (`dlss_jitter_per_eye`, default on: its jitter too), `dlss_fix_motion` (default on) fixes the
/// motion vectors DLSS gets.
pub const STREAMLINE: Component<Beast> = Component {
    name: "streamline",
    on: streamline_on,
    install: install_streamline,
    give_back: || per_eye::ENABLED.store(false, Ordering::Relaxed),
    release: streamline::release,
    ..Component::NONE
};

fn streamline_on(run: &Run<'_, Beast>) -> On {
    let wanted = stereo_on(run.options) || research::options().streamline || run.options.switch("dlss_reset", false);
    On::when(wanted, "no DLSS inputs needed")
}

fn install_streamline(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let r12 = start.build.renderer_12.as_ref().or_missing("the game is not on D3D12")?;
    let options = start.options;
    let reset = options.switch("dlss_reset", false);
    let per_eye = stereo_on(options) && options.switch("dlss_per_eye", false);
    per_eye::FIX_MOTION.store(options.switch("dlss_fix_motion", true), Ordering::Relaxed);
    if per_eye && options.switch("dlss_jitter_per_eye", true) {
        start.hooks.site(&per_eye::FRAME_JITTER, &start.build.renderer, &engine::FRAME_JITTER, per_eye::frame_jitter)?;
        per_eye::JITTER_PER_EYE.store(true, Ordering::Relaxed);
    }
    // SAFETY: rd3d12 is the game's D3D12 renderer DLL.
    unsafe { streamline::install(start.hooks, r12, reset, per_eye, stereo::framegen::enabled()) };
    research::motion::arm();
    Ok(())
}
