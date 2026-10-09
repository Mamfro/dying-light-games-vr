//! Research: the probes that find out how Dying Light 2 works, kept apart from the adapter.
//!
//! The adapter calls in here at the moments a probe watches (`research::cameras::frame(..)` as the
//! frame's camera is read, `research::markers::call(..)` at a HUD marker projection, ...), one line
//! each, and never says which probes listen: each checks its own option ([`options`]) and returns
//! at once when it is off. The options (`probe_*` and the traces that serve them) come from the
//! same options file as the adapter's and are never shown by the launcher.
//!
//! The shared Chrome engine crate has probes of its own (`eng_chr::fpp`, `eng_chr::hudlayer`,
//! `eng_chr::gui`); their switches are read here and handed to it where the adapter installs it.
//!
//! A probe that has found its answer is a candidate for deletion: its finding belongs in the
//! research notes, its facts in `engine.rs`.

pub mod aim;
pub mod cameras;
pub mod markers;
pub mod melee;

use monaka_producer::log;
use std::sync::OnceLock;

/// What this run probes.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// `probe_hands`: log the arms skeleton's elements and the weapon visuals (`eng_chr::fpp`).
    pub hands: bool,
    /// `probe_markers`: log the HUD's world-marker projections and dump the UI layer ([`markers`]).
    pub markers: bool,
    /// `probe_gui`: log the engine's gui documents and their widgets (`eng_chr::gui`).
    pub gui: bool,
    /// `probe_cameras`: log which camera the engine copies into the level camera whenever it
    /// changes: a cutscene's camera against the player's ([`cameras`]).
    pub cameras: bool,
    /// `probe_hud`: log the frames' draw groups and publish the HUD layer made from the game's HUD
    /// draws (`eng_chr::hudlayer`).
    pub hud: bool,
    /// `debug_flags` bit `AIM_LOG`: log head aim and the baked yaw ([`aim`]).
    pub aim_log: bool,
    /// `probe_melee`: log every damage message the game sends ([`melee`]).
    pub melee: bool,
    /// `probe_sweep`: log the melee controller's hit detection and the hits it deals ([`melee`]).
    pub sweep: bool,
}

impl Options {
    pub fn from_options(options: &monaka_core::options::Options) -> Self {
        Self {
            hands: options.switch("probe_hands", false),
            markers: options.switch("probe_markers", false),
            gui: options.switch("probe_gui", false),
            cameras: options.switch("probe_cameras", false),
            hud: options.switch("probe_hud", false),
            aim_log: options.value::<u32>("debug_flags").is_some_and(|bits| bits & crate::config::debug::AIM_LOG != 0),
            melee: options.switch("probe_melee", false),
            sweep: options.switch("probe_sweep", false),
        }
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

/// The stop: what the probes gathered.
pub fn report() {
    cameras::report();
    melee::report();
}

