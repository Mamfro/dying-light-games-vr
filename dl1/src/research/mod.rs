//! Research: the probes that find out how Dying Light 1 works, kept apart from the adapter.
//!
//! The adapter calls in here at the moments a probe watches (`research::look::steered(..)` as head
//! aim steers, `research::hud::draw(..)` at a HUD draw, ...), one line each, and never says
//! which probes listen: each checks its own option ([`options`]) and returns at once when it is
//! off. The options (`probe_*` and the experiments that serve them) come from the same options
//! file as the adapter's and are never shown by the launcher.
//!
//! A probe that has found its answer is a candidate for deletion: its finding belongs in the
//! research notes, its facts in `engine.rs`.

pub mod arms;
pub mod backbuffer;
pub mod bindings;
pub mod bow;
pub mod depth;
pub mod drift;
pub mod give;
pub mod hud;
pub mod layer;
pub mod look;
pub mod melee;
pub mod pad;
pub mod sweep;
pub mod targets;
pub mod throw;
pub mod view;
pub mod walk;
pub mod widget;

use crate::{Dl1, Facts};
use monaka_hook::Hooks;
use monaka_hook::module::Module;
use monaka_producer::Rejection;
use monaka_producer::game::{Lack, Start};
use monaka_producer::research::Research;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// What this run probes.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// `probe_look` (with head aim): how head aim steers the character.
    pub look: bool,
    /// `probe_depth`: the depth buffers bound before the scene reaches the back buffer.
    pub depth: bool,
    /// `probe_backbuffer`: a few frames' back-buffer draws in order, and how many frames had the
    /// 3D image's copy the HUD routing starts from.
    pub backbuffer: bool,
    /// `probe_targets`: every render target drawn into each frame, in order (motion-vector hunting).
    pub targets: bool,
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
    /// `probe_widget=<name>` (with the in-world HUD): the named HUD panel widget's first drawn part at
    /// every snapshot, with the eye being rendered.
    pub widget: Option<String>,
    /// `probe_pad`: what the engine asks SDL about game controllers.
    pub pad: bool,
    /// `probe_layer` (with FSR): every few seconds the back buffer, the upscaled eye and the HUD
    /// layer as PNG, to see what the game drew and where the HUD went.
    pub layer: bool,

    /// `probe_melee`: every hit the game deals (`IControlObject::TakeDamage`): who built it, its bytes.
    pub melee: bool,
    /// `flat=1`: attach for the gameplay probes only, no stereo and nothing drawn differently.
    pub flat: bool,
    /// `flat_buttons=1`, with `flat`: also the VR buttons (without crouch, which follows the head)
    /// and the zombie grabs switch, which need no headset.
    pub flat_buttons: bool,
    /// `probe_hands`: the arms model's elements and where the game puts them against its camera.
    pub hands: bool,
    /// `probe_bow` (with the hand rig): the bow model's elements and which move as it is drawn.
    pub bow: bool,
    /// `probe_fingers`: once, the hands' elements with their rest pose against the animated pose.
    pub fingers: bool,
    /// `probe_sweep`: what the player's melee hit detection sees.
    pub sweep: bool,
    /// `probe_walk` (with the hand rig): the player's walk body as the engine steps it;
    /// `walk_push=x,z` (m/s, world axes) also pushes it that way.
    pub walk: bool,
    pub walk_push: Option<[f32; 2]>,
    /// `probe_bindings`: each input layout the game applies to its bindings, and who asks.
    pub bindings: bool,
    /// `probe_throw`: each throw and drop of an item, and the objects they make.
    pub throw: bool,
    /// `give=Name:count,...` (testing only): items given to the player once, a few seconds into play.
    pub give: Option<String>,
}

impl Options {
    pub fn from_options(options: &monaka_core::options::Options) -> Self {
        Self {
            look: options.switch("probe_look", false),
            depth: options.switch("probe_depth", false),
            backbuffer: options.switch("probe_backbuffer", false),
            targets: options.switch("probe_targets", false),
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
            layer: options.switch("probe_layer", false),

            melee: options.switch("probe_melee", false),
            flat: options.switch("flat", false),
            flat_buttons: options.switch("flat_buttons", false),
            hands: options.switch("probe_hands", false),
            bow: options.switch("probe_bow", false),
            fingers: options.switch("probe_fingers", false),
            sweep: options.switch("probe_sweep", false),
            walk: options.switch("probe_walk", false),
            walk_push: options.floats::<2>("walk_push", 5.0),
            bindings: options.switch("probe_bindings", false),
            give: options.text("give").map(str::to_owned),
            throw: options.switch("probe_throw", false),
        }
    }

    /// The back-buffer draw hooks serve a probe.
    pub fn draws(&self) -> bool {
        self.depth || self.targets || self.backbuffer
    }

    /// The arms callback serves a probe.
    pub fn arms(&self) -> bool {
        self.hands || self.fingers
    }
}

pub static PROBES: Research<Options> = Research::new(Options::from_options);
/// Where probes write files (the DLL copy's folder).
static DIR: OnceLock<PathBuf> = OnceLock::new();

/// This run's probes (none before [`configure`]).
pub fn options() -> &'static Options {
    PROBES.get()
}

/// Where probes write files.
pub fn dir() -> PathBuf {
    DIR.get().cloned().unwrap_or_default()
}

/// Reads the run's probe options; `dir` takes the probes' files.
pub fn configure(read: &monaka_core::options::Options, dir: &Path) {
    PROBES.configure(read);
    let _ = DIR.set(dir.to_path_buf());
    if options().targets {
        targets::enable();
    }
    if let Some(items) = &options().give {
        give::configure(items);
    }
    // Head aim's updates, for `probe_look`, `probe_cutscene` and `probe_frames`.
    monaka_arms::owned_facing::observe(monaka_arms::owned_facing::Observer { traced: look::traced, steered: look::steered });
}

/// The gameplay probes' hooks.
fn install_gameplay(hooks: &mut Hooks, engine_module: &Module, gamedll: &Module) -> Result<(), Rejection> {
    let o = options();
    if o.melee {
        melee::install(hooks, engine_module)?;
    }
    if o.sweep {
        sweep::install(hooks, engine_module, gamedll)?;
    }
    if o.throw {
        throw::install(hooks, gamedll, engine_module)?;
    }
    if o.bindings {
        bindings::install(hooks, gamedll)?;
    }
    if give::wanted() {
        // SAFETY: the game DLL is the build `engine.rs` describes (hash checked); the export's type
        // is `float IGame::GetGameTimeDelta() const`.
        unsafe { eng_chr::providers::game::hook(hooks, gamedll)? };
        eng_chr::providers::game::observe(give::frame);
    }
    Ok(())
}

/// The probes' own hooks, beside the adapter's.
///
/// # Safety
/// The engine and game DLL are the builds `engine.rs` describes (hash checked).
pub unsafe fn install(hooks: &mut Hooks, engine_module: &Module, gamedll: &Module) -> Result<(), Rejection> {
    let o = options();
    install_gameplay(hooks, engine_module, gamedll)?;
    if o.flat {
        return Ok(());
    }
    if o.walk {
        walk::install(hooks, engine_module, o.walk_push)?;
    }
    if o.pad {
        pad::install(hooks, engine_module)?;
        monaka_pad::probe::install(hooks);
    }
    Ok(())
}

/// The probes' hooks: the arms probes take their component's hook when it is off.
pub(crate) fn install_probes(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let probes = options();
    let Facts { engine: engine_module, gamedll, .. } = start.build;
    if !probes.flat && probes.arms() && !start.is_on(&crate::ARMS) {
        crate::providers::skeleton::hook_arms(start.hooks, engine_module, gamedll)?;
    }
    // SAFETY: the engine and the game DLL are the builds `engine.rs` describes (hashes checked).
    unsafe { install(start.hooks, engine_module, gamedll)? };
    Ok(())
}

/// The present thread retires the eye: what the rendering probes gathered.
pub fn finish() {
    drift::report();
    depth::report();
    targets::report();
    backbuffer::report();
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
    if options().bindings {
        bindings::report();
    }
    if options().throw {
        throw::report();
    }
    if options().pad {
        pad::report();
        monaka_pad::probe::report();
    }
}
