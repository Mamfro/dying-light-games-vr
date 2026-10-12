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
pub mod give;
pub mod history;
pub mod motion;
pub mod streamline;

use crate::{Beast, arms, look, stereo_on};
use monaka_hook::Hooks;
use monaka_producer::game::{Lack, Start};
use monaka_hook::module::Module;
use monaka_producer::research::Research;
use monaka_producer::{Rejection, log};
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
    /// `camera_refs=1`: the DLSS command's camera references (`history`).
    pub refs: bool,
    /// `probe_history=1` (with stereo and a synthetic head): where the renderer keeps the previous
    /// frame's camera, found by value (`history`; read-only).
    pub history: bool,
    /// `dump_object=<spec>`, `vp_probe=<spec>`, `frame_probe=1`: see `history`.
    pub dump: Option<String>,
    pub vp: Option<String>,
    pub frames: bool,
    /// `give=Name:count,...` (testing only): items given to the player once, a few seconds into play
    /// (`give`).
    pub give: Option<String>,
}

impl Options {
    pub fn from_options(options: &monaka_core::options::Options) -> Self {
        Self {
            cameras: options.switch("probe", false),
            hands: options.switch("probe_hands", false),
            streamline: options.switch("probe_streamline", false),
            motion: options.switch("probe_motion", false),
            refs: options.switch("camera_refs", false),
            history: options.switch("probe_history", false),
            dump: options.text("dump_object").map(str::to_owned),
            vp: options.text("vp_probe").map(str::to_owned),
            frames: options.switch("frame_probe", false),
            give: options.text("give").map(str::to_owned),
        }
    }

    /// A `history` probe runs (they read the eye positions written).
    pub fn history_any(&self) -> bool {
        self.history || self.refs || self.dump.is_some() || self.vp.is_some() || self.frames
    }
}

/// The probes' install: their hooks on the renderer and the game DLL; `probe_hands` rides on the
/// look's arms callback.
pub(crate) fn install_probes(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let stereo = stereo_on(start.options);
    if options().hands
        && start.is_on(&look::LOOK)
        && !start.is_on(&arms::ARMS)
        && let Err(why) = arms::hands::configure(&start.build.engine, arms::hands_settings(start, false))
    {
        log!("arms probe off: {}", why.reason);
    }
    // SAFETY: the renderer's and the game DLL's hashes were checked.
    unsafe {
        install(start.hooks, &start.build.renderer, stereo)?;
        install_game(start.hooks, start.build.gamedll.as_ref())?;
    }
    self::start(stereo, start.build.renderer.base());
    Ok(())
}

pub static PROBES: Research<Options> = Research::new(Options::from_options);

/// This run's probes (none before the start read them).
pub fn options() -> &'static Options {
    PROBES.get()
}

/// The probes' own hooks, beside the adapter's, on the renderer (`engine::RENDERER`).
///
/// # Safety
/// `renderer` is the build `engine.rs` describes (hash checked).
pub unsafe fn install(hooks: &mut Hooks, renderer: &Module, stereo: bool) -> Result<(), Rejection> {
    let o = options();
    if let Some(items) = &o.give {
        give::configure(items);
    }
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

/// The probes' own hooks on the game DLL (`gamedll`, when the run has it).
///
/// # Safety
/// `gamedll` is the build `engine.rs` describes (hash checked).
pub unsafe fn install_game(hooks: &mut Hooks, gamedll: Option<&Module>) -> Result<(), Rejection> {
    if give::wanted() {
        let Some(gamedll) = gamedll else {
            log!("give: not given: this run does not hook the game DLL (it needs stereo)");
            return Ok(());
        };
        // SAFETY: the caller's guarantee; the import's type is `float IGame::GetGameTimeDelta() const`.
        unsafe { eng_chr::game::hook(hooks, gamedll)? };
        eng_chr::game::observe(give::frame);
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
