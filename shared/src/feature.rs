//! The Chrome Engine's features as ready-made components, for the games on it. A game implements
//! [`Chrome`] once (its modules, its queue, the facts of its build) and lists the components it
//! carries, saying what each works on top of by struct update:
//! `Component { needs: &[&ARMS], ..eng_chr::feature::melee() }`. A fact its build has not been read
//! for leaves that component off for the run, said in the log.

use crate::headaim::{self, HeadAim, IdLoad, InputActionFn, PitchActions};
use crate::{grabs, gui, handwork, hudfix, hudlayer, hudover, melee, melee_reach, panels, view_bob, walk};
use monaka_arms::aim::AimSettings;
use monaka_hook::Site;
use monaka_arms::roomscale;
use monaka_core::feature::Feature;
use monaka_core::manifest::{
    AIM_HAND, AIM_LOOK, AIM_MOTION, AIM_MOUSE, AIM_POINT, DISABLE_VIEW_BOBBING, Kind, Manifest, Opt, ROOM_SCALE, Shown, WORLD_HUD, aim, aims, opt,
};
use monaka_core::options::{Options, Shared};
use monaka_hook::module::Module;
use monaka_producer::game::{Component, Game, Lack, Missing, On, OrMissing, Run, Start, Switch};
use monaka_producer::log;
use std::sync::atomic::{AtomicBool, Ordering::*};
use windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue;

/// A game on the Chrome Engine, as its ready-made components see it.
pub trait Chrome: Game {
    /// What this game's build has been read for.
    const FACTS: Facts;
    /// The engine module.
    fn engine(build: &Self::Build) -> &Module;
    /// The game DLL, when it is the build the facts describe (what needs it stays off otherwise).
    fn gamedll(build: &Self::Build) -> Option<&Module>;
    /// The game's direct queue, once its present request knows it (none on D3D11).
    fn game_queue() -> Option<ID3D12CommandQueue>;
    /// Whether VR shows the game now: the gui tree's pieces and menus count only then.
    fn showing() -> bool;
}

/// One build's facts for the ready-made components (none where the build has not been read).
pub struct Facts {
    pub look: Option<Look>,
    pub view_bob: Option<view_bob::Build>,
    pub grabs: Option<grabs::Build>,
    pub walk: Option<walk::Build>,
    pub melee: Option<melee::Build>,
    /// With physical melee, a weapon's swing attacks only when its blade is about to reach
    /// something.
    pub melee_reach: Option<melee_reach::Build>,
    /// The game starts a light attack when its button is let go (Dying Light 1, The Beast): a swing
    /// presses briefly.
    pub attacks_on_release: bool,
    pub handwork: Option<handwork::Builds>,
    pub gui: Option<gui::Build>,
    /// The HUD's pieces the dynamic HUD takes, by the game's widget names.
    pub pieces: &'static [gui::Piece],
    /// The game's present lays the HUD layer over the eyes whenever the HUD is shown (The Beast);
    /// otherwise the layer is made only for the dynamic HUD (Dying Light 2's eyes draw the HUD in).
    pub hud_over_eyes: bool,
}

/// Head aim's facts: the game's [`HeadAim`] (its offsets), its input-action converter, and the
/// vertical look actions with how the build loads their ids.
pub struct Look {
    pub aim: &'static HeadAim,
    pub input_action: Site<InputActionFn>,
    pub pitch_actions: &'static PitchActions,
    pub id_load: IdLoad,
}

/// The game DLL a component hooks, or why it stays off.
pub fn gamedll<'a, G: Chrome>(start: &Start<'a, G>) -> Result<&'a Module, Lack> {
    G::gamedll(start.build).or_missing("the game DLL is not the inspected build")
}

/// A fact of this build, or why the component needing it stays off.
fn fact<T>(fact: Option<T>, what: &str) -> Result<T, Lack> {
    fact.or_missing(&format!("{what} is not read for this build"))
}

// ---- Look and the arms ----

const LOOK_OPTIONS: &[Opt] = &[aim(&[AIM_MOTION, AIM_LOOK, AIM_POINT, AIM_MOUSE], aims::HAND_RIG), AIM_HAND];

/// Head aim on the character (`headaim`): motion controls, pointing or look-based aim, the
/// controllers in for a pointing hand. A game gives where it steers (`install`), its hand-back
/// (`give_back`, its `HeadAim`'s release) and what it works on top of, by struct update.
pub const fn look<G: Chrome>() -> Component<G> {
    Component {
        name: "look",
        features: &[Feature::Look],
        options: LOOK_OPTIONS,
        needs_channel: true,
        on: |run| On::when(run.shared.aim.head, "aim is mouse and stick only"),
        install: install_look::<G>,
        give_back: give_back_look::<G>,
        release: monaka_arms::controllers::close,
        ..Component::NONE
    }
}

/// Head aim's own part of a look's install: what aims, the vertical look input held while the head
/// steers, the controllers for a pointing hand. A game's install calls it, then hooks where it
/// steers ([`HeadAim::steer`]).
pub fn install_look<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let look = fact(G::FACTS.look, "head aim")?;
    let settings = AimSettings::from_options(start.options, start.shared.aim, true);
    look.aim.configure(settings, gamedll(start)?.base());
    log!("{settings:?}");
    match hook_input_action::<G>(start) {
        Err(Lack::Missing(why)) => log!("vertical look not locked: {why}"),
        other => other?,
    }
    if settings.hand_aim {
        monaka_arms::controllers::open(start.channel()?);
    }
    Ok(())
}

/// The game's input-action converter hooked ([`headaim::input_action`]), once the vertical look
/// actions are where the build keeps them.
pub fn hook_input_action<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let look = fact(G::FACTS.look, "head aim")?;
    let gamedll = gamedll(start)?;
    headaim::pitch_actions_match(G::engine(start.build), look.pitch_actions, look.id_load).missing()?;
    headaim::convert_for(look.aim, look.pitch_actions);
    start.hooks.site(&headaim::INPUT_ACTION, gamedll, &look.input_action, headaim::input_action)?;
    Ok(())
}

/// The next camera update hands the head's share back.
fn give_back_look<G: Chrome>() {
    if let Some(look) = G::FACTS.look {
        look.aim.release(500);
    }
}

/// The arms on the controllers (`rig`), with motion controls: a game gives its install and what it
/// works on top of, by struct update.
pub const fn arms<G: Chrome>() -> Component<G> {
    Component { name: "arms", features: &[Feature::HandRig], on: |run| On::when(run.shared.aim.rig, "aim is not motion controls"), ..Component::NONE }
}

// ---- View bob ----

/// The view stays level instead of bobbing with the steps (Monaka's own setting): the player's
/// head bob held at zero while head aim steers the character (`view_bob`, held at each of its
/// updates), so a game lists it on top of its look.
pub const fn view_bob<G: Chrome>() -> Component<G> {
    Component {
        name: "view bob",
        options: &[DISABLE_VIEW_BOBBING],
        switch: Some(Switch::new("disable_view_bobbing", true, "the view bobs as in the game")),
        install: install_view_bob::<G>,
        ..Component::NONE
    }
}

fn install_view_bob<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    view_bob::install(gamedll(start)?, fact(G::FACTS.view_bob, "the head bob")?);
    Ok(())
}

// ---- Grabs ----

pub const DISABLE_ZOMBIE_GRABS: Opt =
    opt("disable_zombie_grabs", "Disable zombie grabs", "Zombies never grab you. Off: they grab and bite until you break free, as in the game.", Kind::Switch { default: true })
        .on(Shown::Game);

/// Grabs never land on the player (`grabs`).
pub const fn grabs<G: Chrome>() -> Component<G> {
    Component {
        name: "grabs",
        options: &[DISABLE_ZOMBIE_GRABS],
        switch: Some(GRABS_SWITCH),
        install: install_grabs::<G>,
        release: grabs::report,
        ..Component::NONE
    }
}

/// Zombies' grabs left as they are (`disable_zombie_grabs=0`).
pub const GRABS_SWITCH: Switch = Switch::new("disable_zombie_grabs", true, "zombies grab as in the game");

fn install_grabs<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let gamedll = gamedll(start)?;
    grabs::install(start.hooks, gamedll, fact(G::FACTS.grabs, "the grab conditions")?).missing()
}

// ---- Room-scale ----

/// The character follows the player's real steps (`monaka_arms::roomscale` at the player's
/// movement step, `walk`), with the hand rig.
pub const fn room_scale<G: Chrome>() -> Component<G> {
    Component {
        name: "room scale",
        features: &[Feature::RoomScale],
        options: &[ROOM_SCALE],
        settings: read_room_scale,
        on: room_scale_on::<G>,
        install: install_room_scale::<G>,
        quiet: roomscale::stop,
        release: walk::report,
        ..Component::NONE
    }
}

fn read_room_scale(options: &Options, manifest: &Manifest) {
    roomscale::Settings::from_options(options, &Shared::from_options(options, manifest));
}

fn room_scale_on<G: Chrome>(run: &Run<'_, G>) -> On {
    On::when(roomscale::Settings::from_options(run.options, run.shared).is_some(), "room-scale movement is off")
}

fn install_room_scale<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let settings = fact(roomscale::Settings::from_options(start.options, start.shared), "room-scale movement")?;
    let gamedll = gamedll(start)?;
    walk::install(start.hooks, G::engine(start.build), gamedll, fact(G::FACTS.walk, "the movement step")?).missing()?;
    roomscale::install(settings);
    Ok(())
}

// ---- Physical melee ----

/// Physical melee (`melee`): a swing hits where the weapon in the hand is, through the game's own
/// hit detection; with the game's reach check (`melee_reach`), a weapon's swing attacks only when
/// its blade is about to reach something (`melee_on_reach=0` lets every swing attack, for a test).
/// The swing that presses the attack is the hand rig's.
pub const fn melee<G: Chrome>() -> Component<G> {
    Component { name: "melee", features: &[Feature::Melee], on: melee_on::<G>, install: install_melee::<G>, release: melee_reach::report, ..Component::NONE }
}

fn physical<G: Chrome>(options: &Options, shared: &Shared) -> Option<monaka_arms::melee::Physical> {
    monaka_arms::melee::Physical::from_options(options, shared.aim.rig, G::FACTS.attacks_on_release)
}

fn melee_on<G: Chrome>(run: &Run<'_, G>) -> On {
    On::when(physical::<G>(run.options, run.shared).is_some(), "physical melee is off")
}

fn install_melee<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let physical = fact(physical::<G>(start.options, start.shared), "physical melee")?;
    let gamedll = gamedll(start)?;
    melee::install(start.hooks, gamedll, fact(G::FACTS.melee, "the melee code")?, physical).missing()?;
    if let Some(reach) = G::FACTS.melee_reach
        && start.options.switch("melee_on_reach", true)
        && let Err(why) = melee_reach::install(G::engine(start.build), gamedll, reach)
    {
        log!("every swing attacks: {}", why.reason);
    }
    Ok(())
}

// ---- Handwork ----

/// What the hands do with the game's own throw, bow, reload and lockpick (`handwork`), with the
/// hand rig. Each part is the rig's, not a setting: its option stays readable for tests only.
pub const fn handwork<G: Chrome>() -> Component<G> {
    Component { name: "handwork", features: handwork_features::<G>(), on: handwork_on::<G>, install: install_handwork::<G>, release: handwork::report, ..Component::NONE }
}

/// The parts this build has been read for.
const fn handwork_features<G: Chrome>() -> &'static [Feature] {
    match G::FACTS.handwork {
        Some(handwork::Builds { lockpick: Some(_), .. }) => &[Feature::Throw, Feature::Bow, Feature::Reload, Feature::Lockpick],
        Some(_) => &[Feature::Throw, Feature::Bow, Feature::Reload],
        None => &[],
    }
}

/// `motion_throw=0`, `motion_bow=0` (`bow_string=0`: only the string drawn by hand),
/// `manual_reload=0`, `motion_lockpick=0` turn one part off, for a test.
fn handwork_options(options: &Options, rig: bool) -> handwork::Options {
    handwork::Options {
        throw: options.switch("motion_throw", true) && rig,
        bow: options.switch("motion_bow", true) && rig,
        bow_string: options.switch("bow_string", true),
        reload: options.switch("manual_reload", true) && rig,
        lockpick: options.switch("motion_lockpick", true) && rig,
    }
}

fn handwork_on<G: Chrome>(run: &Run<'_, G>) -> On {
    On::when(handwork_options(run.options, run.shared.aim.rig) != handwork::Options::default(), "aim is not motion controls")
}

fn install_handwork<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let builds = fact(G::FACTS.handwork, "the hands' actions")?;
    let gamedll = gamedll(start)?;
    handwork::install(start.hooks, gamedll, &builds, handwork_options(start.options, start.shared.aim.rig));
    Ok(())
}

// ---- HUD ----

/// The gui tree is hooked this run (the dynamic HUD's pieces can be read).
static GUI: AtomicBool = AtomicBool::new(false);
/// The dynamic HUD runs: the pieces go onto the hands at each present.
static PANELS: AtomicBool = AtomicBool::new(false);

/// Whether the dynamic HUD runs ([`panels`]).
pub fn panels_on() -> bool {
    PANELS.load(Acquire)
}

/// The HUD as a layer of its own, from the game's command lists (`hudlayer`; `probe_hud=1` probes
/// it): laid over the eyes in front of them, or kept for the dynamic HUD's pieces. The gui tree
/// comes with it (`gui`; `probe_gui=1` probes it), telling when a menu is open (shown flat).
pub const fn hud<G: Chrome>() -> Component<G> {
    Component { name: "hud", features: &[Feature::HudLayer], on: hud_on::<G>, install: install_hud::<G>, give_back: remove_hud, release: close_hud, ..Component::NONE }
}

fn hud_on<G: Chrome>(run: &Run<'_, G>) -> On {
    let hud = run.shared.hud;
    On::when(hud.world || (G::FACTS.hud_over_eyes && hud.shown), "the HUD stays as the game draws it")
}

fn install_hud<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let queue = G::game_queue().or_missing("the game queue is not known (D3D11, or no present yet)")?;
    let hud = start.shared.hud;
    // Kept out of the frame where it is laid back over the eyes; drawn in too where only its
    // pieces are taken out.
    let redirect = hud.shown || !G::FACTS.hud_over_eyes;
    // SAFETY: the game's live direct queue.
    unsafe { hudlayer::install(start.hooks, &queue, redirect, start.options.switch("probe_hud", false), start.channel().ok().cloned()) }.missing()?;
    let probe_gui = start.options.switch("probe_gui", false);
    let gui = match G::FACTS.gui {
        Some(build) => gui::install(start.hooks, G::engine(start.build), &build).map_err(|why| why.reason),
        None => Err("not read for this build".to_owned()),
    };
    match gui {
        Ok(()) => {
            gui::configure(probe_gui, &[], G::showing);
            GUI.store(true, Release);
        }
        Err(why) => log!("gui tree unavailable (no dynamic HUD, menus not shown flat): {why}"),
    }
    Ok(())
}

fn remove_hud() {
    hudlayer::remove(1000);
}

fn close_hud() {
    GUI.store(false, Release);
    panels::close();
    hudfix::close();
    hudover::close();
    gui::report();
}

/// The dynamic HUD: the HUD's pieces read from the gui tree and put on the hands, out of the eyes.
pub const fn panels<G: Chrome>() -> Component<G> {
    Component {
        name: "panels",
        features: &[Feature::HudPanels],
        options: &[WORLD_HUD],
        on: |run| On::when(run.shared.hud.world, "the dynamic HUD is off"),
        install: install_panels::<G>,
        arm: || PANELS.store(true, Release),
        quiet: || PANELS.store(false, Release),
        ..Component::NONE
    }
}

fn install_panels<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    GUI.load(Acquire).then_some(()).or_missing("no gui tree to read the pieces from")?;
    gui::configure(start.options.switch("probe_gui", false), G::FACTS.pieces, G::showing);
    Ok(())
}
