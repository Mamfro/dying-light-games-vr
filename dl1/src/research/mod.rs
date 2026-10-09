//! Research: the probes that find out how Dying Light 1 works, kept apart from the adapter.
//!
//! The adapter calls in here at the moments a probe watches (`research::look::seen(..)` in the
//! camera hook, `research::hud::draw(..)` at a HUD draw, ...), one line each, and never says
//! which probes listen: each checks its own option ([`options`]) and returns at once when it is
//! off. The options (`probe_*` and the experiments that serve them) come from the same options
//! file as the adapter's and are never shown by the launcher.
//!
//! A probe that has found its answer is a candidate for deletion: its finding belongs in the
//! research notes, its facts in `engine.rs`.

pub mod arms;
pub mod depth;
pub mod drift;
pub mod hud;
pub mod look;
pub mod melee;
pub mod pad;
pub mod sweep;
pub mod targets;
pub mod video;
pub mod view;
pub mod walk;
pub mod widget;

use monaka_hook::Hooks;
use monaka_hook::module::Module;
use monaka_producer::{Rejection, enable_hooks, log, require_build};
use std::path::PathBuf;
use std::sync::OnceLock;

/// What this run probes.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// `probe_look`: the player camera's call sites, the classes around it and the fields holding
    /// its pitch (look up and down with the mouse before attaching).
    pub look: bool,
    /// `probe_depth`: the depth buffers bound before the scene reaches the back buffer.
    pub depth: bool,
    /// `probe_targets`: every render target drawn into each frame, in order (motion-vector hunting).
    pub targets: bool,
    /// `probe_video`: where the game keeps the current resolution.
    pub video: bool,
    /// `probe_frames`: the camera each view setup sees, for 1500 calls (flicker hunting).
    pub frames: bool,
    /// `probe_rig`: for 600 eye frames, where the hand rig's palm sits against each eye and how far
    /// the rig's tracking origin is from the view's (flicker hunting).
    pub rig: bool,
    /// `probe_cutscene`: once the game's camera first moves (a paused cutscene let go), each eye
    /// camera and each look update for a few seconds (cutscene flicker hunting).
    pub cutscene: bool,
    /// `probe_hud`: one frame's HUD draws in order, their geometry, and the panels dumped.
    pub hud: bool,
    /// `hud_hide=a-b`: the frame's HUD draws `a` to `b` (counted from 0) left undrawn.
    pub hud_hide: Option<(u32, u32)>,
    /// `probe_widget=<name>` (with the in-world HUD): the named HUD widget's first drawn part at
    /// every snapshot, with the eye being rendered.
    pub widget: Option<String>,
    /// `probe_pad`: what the engine asks SDL about game controllers.
    pub pad: bool,
    /// `probe_melee`: every hit the game deals (`IControlObject::TakeDamage`): who built it, its bytes.
    pub melee: bool,
    /// `flat=1`: attach for the gameplay probes only, no stereo and nothing drawn differently.
    pub flat: bool,
    /// `probe_hands`: the arms model's elements and where the game puts them against its camera.
    pub hands: bool,
    /// `probe_fingers`: once, the hands' elements with their rest pose against the animated pose.
    pub fingers: bool,
    /// `probe_sweep`: what the player's melee hit detection sees.
    pub sweep: bool,
    /// `probe_walk` (with the hand rig): the player's walk body as the engine steps it;
    /// `walk_push=x,z` (m/s, world axes) also pushes it that way.
    pub walk: bool,
    pub walk_push: Option<[f32; 2]>,
}

impl Options {
    pub fn from_options(options: &monaka_core::options::Options) -> Self {
        Self {
            look: options.switch("probe_look", false),
            depth: options.switch("probe_depth", false),
            targets: options.switch("probe_targets", false),
            video: options.switch("probe_video", false),
            frames: options.switch("probe_frames", false),
            rig: options.switch("probe_rig", false),
            cutscene: options.switch("probe_cutscene", false),
            hud: options.switch("probe_hud", false),
            hud_hide: options.text("hud_hide").and_then(|t| {
                let (first, last) = t.split_once('-')?;
                Some((first.trim().parse().ok()?, last.trim().parse().ok()?))
            }),
            widget: options.text("probe_widget").map(str::to_owned),
            pad: options.switch("probe_pad", false),
            melee: options.switch("probe_melee", false),
            flat: options.switch("flat", false),
            hands: options.switch("probe_hands", false),
            fingers: options.switch("probe_fingers", false),
            sweep: options.switch("probe_sweep", false),
            walk: options.switch("probe_walk", false),
            walk_push: options.floats::<2>("walk_push", 5.0),
        }
    }

    /// The back-buffer draw hooks serve a probe.
    pub fn draws(&self) -> bool {
        self.depth || self.targets
    }

    /// The arms callback serves a probe.
    pub fn arms(&self) -> bool {
        self.hands || self.fingers
    }
}

static OPTIONS: OnceLock<Options> = OnceLock::new();
/// Where probes write files (the DLL copy's folder).
static DIR: OnceLock<PathBuf> = OnceLock::new();

/// This run's probes (none before [`configure`]).
pub fn options() -> &'static Options {
    OPTIONS.get_or_init(Options::default)
}

/// Where probes write files.
pub fn dir() -> PathBuf {
    DIR.get().cloned().unwrap_or_default()
}

/// Reads the run's probe options; `dir` takes the probes' files.
pub fn configure(options: &monaka_core::options::Options, dir: &std::path::Path) {
    let options = Options::from_options(options);
    if options.targets {
        targets::enable();
    }
    log!("research: {options:?}");
    let _ = OPTIONS.set(options);
    let _ = DIR.set(dir.to_path_buf());
}

/// `flat=1`: only the gameplay probes' hooks, on the game as it plays on the monitor. `None`
/// without `flat`.
pub fn flat(engine_module: &Module) -> Option<Result<(), Rejection>> {
    options().flat.then(|| {
        let mut hooks = Hooks::default();
        install_gameplay(&mut hooks, engine_module)?;
        log!("flat: probes only, no stereo");
        enable_hooks(&crate::HOOKS, hooks)
    })
}

/// The gameplay probes' hooks.
fn install_gameplay(hooks: &mut Hooks, engine_module: &Module) -> Result<(), Rejection> {
    let o = options();
    if o.melee {
        melee::install(hooks, engine_module)?;
    }
    if o.sweep {
        let gamedll = require_build(crate::engine::GAMEDLL, crate::engine::GAMEDLL_SHA256)?;
        sweep::install(hooks, engine_module, &gamedll)?;
    }
    Ok(())
}

/// The probes' own hooks, beside the adapter's; `size` is the back buffer's.
///
/// # Safety
/// The engine and game DLL are the builds `engine.rs` was measured in (hash checked).
pub unsafe fn install(hooks: &mut Hooks, engine_module: &Module, size: (u32, u32)) -> Result<(), Rejection> {
    let o = options();
    if o.video {
        video::set_size(size.0, size.1);
        let gamedll = require_build(crate::engine::GAMEDLL, crate::engine::GAMEDLL_SHA256)?;
        // SAFETY: the caller's guarantee; the export's type is `float IGame::GetGameTimeDelta() const`.
        unsafe { eng_chr::game::hook(hooks, &gamedll)? };
        eng_chr::game::observe(video::probe_frame);
    }
    install_gameplay(hooks, engine_module)?;
    if o.walk {
        walk::install(hooks, engine_module, o.walk_push)?;
    }
    if o.pad {
        pad::install(hooks, engine_module)?;
        monaka_pad::probe::install(hooks);
    }
    Ok(())
}

/// The present thread retires the eye: what the rendering probes gathered.
pub fn finish() {
    drift::report();
    look::report();
    depth::report();
    targets::report();
}

/// After the hooks are off.
pub fn release() {
    targets::release();
}

/// The stop: what the gameplay probes gathered.
pub fn report() {
    widget::report();
    melee::report();
    sweep::report();
    walk::report();
    if options().pad {
        pad::report();
        monaka_pad::probe::report();
    }
}
