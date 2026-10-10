//! Research: the probes that find out how Dying Light: The Beast works, kept apart from the adapter.
//!
//! The adapter calls in here at the moments a probe watches (`research::cameras::frustum(..)` in
//! the camera hook, `research::history::shown(..)` at a present, ...), one line each, and never
//! says which probes listen: each checks its own option ([`options`]) and returns at once when it
//! is off. The options (`probe_*` and the experiments that serve them) come from the same options
//! file as the adapter's and are never shown by the launcher.
//!
//! A probe that has found its answer is a candidate for deletion: its finding belongs in the
//! research notes, its facts in `engine.rs`.

pub mod cameras;
pub mod history;
pub mod motion;
pub mod streamline;

use monaka_hook::Hooks;
use monaka_hook::module::Module;
use monaka_producer::{Rejection, log};
use std::sync::OnceLock;
use std::time::Duration;

/// What this run probes.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// `probe=1`: who sets and rebuilds which renderer camera, every two seconds (`cameras`).
    pub cameras: bool,
    /// `probe_hands=1` (with stereo) logs the arms skeleton ([`eng_chr::rig`]).
    pub hands: bool,
    /// `probe_streamline=1` (D3D12): the DLSS inputs (tags and camera constants) of the first
    /// frames (`streamline`; read-only).
    pub streamline: bool,
    /// `probe_motion=1`: the motion vectors' statistics (`motion`).
    pub motion: bool,
    /// The dynamic HUD's probes in `eng_chr` (`probe_hud`: [`eng_chr::hudlayer`], `probe_gui`:
    /// [`eng_chr::gui`]).
    pub hud: bool,
    pub gui: bool,
    /// `camera_refs=1`: the DLSS command's camera references (`history`).
    pub refs: bool,
    /// `probe_history=1` (with stereo and a synthetic head): where the renderer keeps the previous
    /// frame's camera, found by value (`history`; read-only).
    pub history: bool,
    /// `dump_object=<spec>`, `vp_probe=<spec>`, `frame_probe=1`: see `history`.
    pub dump: Option<String>,
    pub vp: Option<String>,
    pub frames: bool,
}

impl Options {
    pub fn from_options(options: &monaka_core::options::Options) -> Self {
        Self {
            cameras: options.switch("probe", false),
            hands: options.switch("probe_hands", false),
            streamline: options.switch("probe_streamline", false),
            motion: options.switch("probe_motion", false),
            hud: options.switch("probe_hud", false),
            gui: options.switch("probe_gui", false),
            refs: options.switch("camera_refs", false),
            history: options.switch("probe_history", false),
            dump: options.text("dump_object").map(str::to_owned),
            vp: options.text("vp_probe").map(str::to_owned),
            frames: options.switch("frame_probe", false),
        }
    }

    /// A `history` probe runs (they read the eye positions written).
    pub fn history_any(&self) -> bool {
        self.history || self.refs || self.dump.is_some() || self.vp.is_some() || self.frames
    }
}

static OPTIONS: OnceLock<Options> = OnceLock::new();

/// This run's probes (none before [`configure`]).
pub fn options() -> &'static Options {
    OPTIONS.get_or_init(Options::default)
}

/// Reads the run's probe options.
pub fn configure(options: &monaka_core::options::Options) {
    let options = Options::from_options(options);
    log!("research: {options:?}");
    let _ = OPTIONS.set(options);
}

/// The probes' own hooks, beside the adapter's, on the renderer (`engine::RENDERER`).
///
/// # Safety
/// `renderer` is the build `engine.rs` describes (hash checked).
pub unsafe fn install(hooks: &mut Hooks, renderer: &Module, stereo: bool) -> Result<(), Rejection> {
    let o = options();
    if o.cameras {
        // SAFETY: the caller's guarantee.
        unsafe { cameras::install(hooks, renderer)? };
    }
    if stereo && o.refs {
        // SAFETY: the caller's guarantee.
        unsafe { history::install_refs(hooks, renderer)? };
    }
    Ok(())
}

/// The hooks are in: the probes that start from an address arm (with stereo).
pub fn start(stereo: bool, renderer_base: usize) {
    history::start(stereo, renderer_base);
}

/// The reporter thread's tick (`elapsed` since it started): the scans once due, and what the
/// probes have gathered once they have it.
pub fn poll(elapsed: Duration) {
    history::scans(elapsed);
    history::report_if_done();
    history::report_dump();
    history::report_vp();
    motion::report();
    history::report_frames();
}

/// Every reporting period of `seconds`.
pub fn periodic(seconds: f64) {
    if options().cameras {
        cameras::report(Some(seconds));
    }
}

/// The stop, after the hooks are out.
pub fn report() {
    if options().cameras {
        cameras::report(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_are_off_unless_asked() {
        let none = Options::from_options(&monaka_core::options::Options::parse(""));
        assert!(!none.cameras && !none.streamline && !none.motion && !none.history_any());
        let some = Options::from_options(&monaka_core::options::Options::parse("probe=1\nprobe_motion=1\ndump_object=10:20"));
        assert!(some.cameras && some.motion && some.history_any());
        assert_eq!(some.dump.as_deref(), Some("10:20"));
    }
}
