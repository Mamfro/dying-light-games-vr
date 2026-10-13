//! The Chrome Engine's features as ready-made components, for the games on it. A game implements
//! [`Chrome`] once (its modules, its queue, the facts of its build) and lists the components it
//! carries, saying what each works on top of by struct update:
//! `Component { needs: &[&HAND_RIG], ..eng_chr::feature::melee() }`. A fact its build has not been read
//! for leaves that component off for the run, said in the log.

use crate::providers::look_fields::HeadAim;
use crate::providers::look_fields::{self, IdLoad, InputActionFn, PitchActions};
use crate::providers::{actions, grabs, gui, hudlayer, model, rig, view_bob, walk};
use monaka_body::hand_rig;
use monaka_body::held::HeldItems;
use monaka_body::reload::Reloads;
use monaka_hook::Site;
use monaka_core::manifest::{DISABLE_VIEW_BOBBING, Kind, Opt, Shown, opt};
use monaka_hook::module::Module;
use monaka_producer::game::{Component, Lack, Missing, On, OrMissing, Run, Start, Switch};
use monaka_producer::log;
use monaka_producer::situation::Situation;
use monaka_stereo::hud_panels::LayerPieces;
use monaka_stereo::providers::GameQueue;
use std::sync::atomic::{AtomicBool, Ordering::*};

/// A game on the Chrome Engine, as its ready-made components see it (its hand rig Monaka's,
/// [`crate::providers::fpp::chrome_rig`], with what the hands hold, [`crate::providers::held`], and
/// the bow drawn and the melee's reach looked at at its posing moment, [`crate::providers::bow`],
/// [`crate::providers::melee`]; its direct queue once its present
/// request knows it, none on D3D11).
pub trait Chrome: HeldItems + monaka_body::bow::Bows + monaka_body::melee_hit::Hits + GameQueue {
    /// What this game's build has been read for.
    const FACTS: Facts;
    /// The engine module.
    fn engine(build: &Self::Build) -> &Module;
    /// The game DLL, when it is the build the facts describe (what needs it stays off otherwise).
    fn gamedll(build: &Self::Build) -> Option<&Module>;
    /// Whether VR shows the game now: the gui tree's pieces and menus count only then.
    fn showing() -> bool;
}

/// One build's facts for the ready-made components (none where the build has not been read).
pub struct Facts {
    pub look: Option<Look>,
    pub arms: Option<rig::Arms>,
    pub view_bob: Option<view_bob::Build>,
    pub grabs: Option<grabs::Build>,
    pub walk: Option<walk::Build>,
    pub melee: Option<crate::providers::melee::Build>,
    /// With physical melee, a weapon's swing attacks only when its blade is about to reach
    /// something.
    pub melee_reach: Option<crate::providers::melee::Targets>,
    /// The game starts a light attack when its button is let go (Dying Light 1, The Beast): a swing
    /// presses briefly.
    pub attacks_on_release: bool,
    /// What the hands do with the game's own throw, bow and lockpick.
    pub hands: Option<crate::providers::hands::Builds>,
    /// The gun's update and reload start, for the reload taken the game's own way.
    pub reload: Option<actions::Reload>,
    pub gui: Option<gui::Build>,
    /// The HUD's pieces the dynamic HUD takes, by the game's widget names (in the order of its
    /// `monaka_stereo::hud_panels::HudPieces::PIECES`: [`gui::panels`]).
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
    /// The engine exports its cutscene query (`look_fields::IS_OBJECT_ON_SOME_MOVIE`): head aim
    /// stands down while a cutscene has the player ([`look_fields::Cutscene`]). Without it head aim
    /// steers through cutscenes.
    pub cutscenes: bool,
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

/// Another camera taking the view, where the engine says when a cutscene has the player: what a
/// Chrome game's look by targets reports (`monaka_body::look::Looks::REPORTS`).
pub const fn look_reports<G: Chrome>() -> &'static [Situation] {
    match G::FACTS.look {
        Some(Look { cutscenes: true, .. }) => &[Situation::CameraTaken],
        _ => &[],
    }
}

/// A Chrome game's look provider install for the look by targets
/// (`monaka_body::look::Looks::install`, what aims already set): the game DLL its
/// character is recognised in, the vertical look input held while the head steers, the cutscene
/// query, and the arms visual's callback where head aim steers there. A game whose head aim steers
/// elsewhere hooks that after it ([`HeadAim::steer`]).
pub fn install_look<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let look = fact(G::FACTS.look, "head aim")?;
    look.aim.set_gamedll(gamedll(start)?.base());
    match hook_input_action::<G>(start) {
        Err(Lack::Missing(why)) => log!("vertical look not locked: {why}"),
        other => other?,
    }
    if look.cutscenes {
        look.aim.cutscene().resolve(G::engine(start.build)).missing()?;
    } else if look.aim.fields().camera_sources.is_some() {
        log!("head aim: stands down while an animation drives the camera (the game's own flag; the engine has no movie query)");
    } else {
        log!("head aim: steers through cutscenes (the engine does not export IsObjectOnSomeMovie)");
    }
    if let Some(arms) = G::FACTS.arms
        && arms.head_aim_here
    {
        // SAFETY: the game DLL is the build the facts describe.
        unsafe { rig::hook::<G>(start.hooks, gamedll(start)?) }.missing()?;
    }
    Ok(())
}

/// The game's input-action converter hooked ([`look_fields::input_action`]), once the vertical look
/// actions are where the build keeps them: where head aim holds vertical look neutral, and the pitch
/// lock (`monaka_body::pitch_lock`) asks.
pub fn hook_input_action<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let look = fact(G::FACTS.look, "head aim")?;
    let gamedll = gamedll(start)?;
    look_fields::pitch_actions_match(G::engine(start.build), look.pitch_actions, look.id_load).missing()?;
    look_fields::convert_for(look.aim, look.pitch_actions);
    start.hooks.site(&look_fields::INPUT_ACTION, gamedll, &look.input_action, look_fields::input_action)?;
    Ok(())
}

/// A Chrome game's look provider after the hooks are out
/// (`monaka_body::look::Looks::release`): the arms' tally where head aim steered at
/// their callback.
pub fn release_look<G: Chrome>() {
    if let Some(arms) = G::FACTS.arms
        && arms.head_aim_here
    {
        arms.rig.report();
    }
}

/// The arms on the controllers at the arms visual's callback (`rig`), with motion controls, a swing
/// of a held weapon or a fist attacking (`swing_attack`, `swing_speed`): a game gives what it works
/// on top of, by struct update.
pub const fn hand_rig<G: Chrome>() -> Component<G> {
    Component { install: |start| install_rig(start, true), release: release_hand_rig::<G>, ..hand_rig::hand_rig() }
}

/// The arms visual's callback for the run, posing the arms (`rig`) or only probing them
/// (`probe_hands=1`): hooked here, unless head aim steers there and hooked it.
pub fn install_rig<G: Chrome>(start: &mut Start<'_, G>, rig: bool) -> Result<(), Lack> {
    let arms = fact(G::FACTS.arms, "the arms")?;
    if rig {
        hand_rig::install::<G>(start)?;
    }
    let options = rig::Options::read(start.options, start.shared, rig, start.options.switch("probe_hands", false));
    arms.rig.configure(G::engine(start.build), options).missing()?;
    // The held weapons' models, for the bow's line and the blade's reach.
    if let Err(why) = model::resolve(G::engine(start.build), &model::VIRTUAL) {
        log!("model elements: {why}");
    }
    if !arms.head_aim_here {
        // SAFETY: the game DLL is the build the facts describe.
        unsafe { rig::hook::<G>(start.hooks, gamedll(start)?) }.missing()?;
    }
    Ok(())
}

fn release_hand_rig<G: Chrome>() {
    if let Some(arms) = G::FACTS.arms
        && !arms.head_aim_here
    {
        arms.rig.report();
    }
    monaka_body::controllers::close();
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
    opt("disable_zombie_grabs", "Disable zombie grabs", "Off: zombies grab you as in the game.", Kind::Switch { default: true })
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

/// The player's movement step hooked for room-scale following (`walk`): a game's
/// `monaka_body::feature::Steps::install`, under `monaka_body::feature::room_scale()`.
pub fn hook_steps<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let gamedll = gamedll(start)?;
    walk::install(start.hooks, G::engine(start.build), gamedll, fact(G::FACTS.walk, "the movement step")?).missing()
}

// ---- Physical melee ----

/// Physical melee (`monaka_body::melee_hit`) on the game's hit detection
/// ([`crate::providers::melee`]): a swing hits where the weapon in the hand is; where the build has
/// been read for the melee's targets (`melee_reach`), a weapon's swing attacks only when its blade
/// is about to reach something (`melee_on_reach=0` lets every swing attack, for a test). The swing
/// that presses the attack is the hand rig's.
pub const fn melee<G: Chrome>() -> Component<G> {
    monaka_body::melee_hit::physical_melee()
}

// ---- Throw, bow and lockpick ----

/// The bow by hand (`monaka_body::bow`) on the bow's fire hook and draw ([`crate::providers::bow`]):
/// `motion_bow=0` turns it off, `bow_string=0` leaves the bow model's string to the game, for a
/// test. A game says what it works on top of by struct update.
pub const fn bow<G: Chrome>() -> Component<G> {
    Component { switch: Some(Switch::new("motion_bow", true, "the game's bow")), ..monaka_body::bow::bow() }
}

/// Throwing by hand (`monaka_body::throw`) on the game's throw release
/// ([`crate::providers::throws`]): `motion_throw=0` turns it off, for a test. A game says what it
/// works on top of by struct update.
pub const fn throw<G: Chrome + monaka_body::throw::ThrowsByHand>() -> Component<G> {
    Component { switch: Some(Switch::new("motion_throw", true, "the game's throw")), ..monaka_body::throw::throw() }
}

/// Lockpicking by hand (`monaka_body::lockpick`), the minigame taken the game's own way
/// ([`crate::providers::lockpick`]): `motion_lockpick=0` turns it off, for a test. A game lists it
/// where its build has been read for the lockpick, saying what it works on top of by struct update.
pub const fn lockpick<G: Chrome + monaka_body::lockpick::Lockpicks>() -> Component<G> {
    Component { switch: Some(Switch::new("motion_lockpick", true, "the game's lockpick")), ..monaka_body::lockpick::lockpick() }
}

// ---- Reload ----

/// The player's gun's update hooked ([`actions::install_reload`]): the reload taken the game's own
/// way, and the moment reloading by hand reads its gesture there (`monaka_body::reload::update`).
/// A game's `Reloads::install`, under `monaka_body::reload::reload()`.
pub fn hook_reload<G: Chrome + Reloads>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let gamedll = gamedll(start)?;
    actions::install_reload(start.hooks, gamedll, fact(G::FACTS.reload, "the gun's reload")?, monaka_body::reload::update::<G>).missing()
}

// ---- HUD ----

/// The gui tree is hooked this run (the dynamic HUD's pieces can be read).
static GUI: AtomicBool = AtomicBool::new(false);

/// The HUD as a layer of its own, from the game's command lists (`hudlayer`; `probe_hud=1` probes
/// it): laid over the eyes in front of them (by the lay-over, `monaka_stereo::hud_layer::present12`,
/// where the game's present entry runs it), or kept for the dynamic HUD's pieces. The gui tree
/// comes with it (`gui`; `probe_gui=1` probes it), telling when a menu is open (shown flat). On the
/// HUD layer's base (`monaka_stereo::hud_layer`: its size and distance).
pub const fn hud<G: Chrome>() -> Component<G> {
    Component { on: hud_on::<G>, install: install_hud::<G>, give_back: remove_hud, release: close_hud, ..monaka_stereo::hud_layer::hud_layer() }
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
    hudlayer::leave();
    GUI.store(false, Release);
    monaka_stereo::hudover12::close();
    gui::report();
}

/// The dynamic HUD (Monaka's `monaka_stereo::hud_panels`, by layer crops): the HUD's pieces read
/// from the gui tree ([`gui::boxes`], the game's `LayerPieces`) and put on the hands, out of the
/// eyes. A game lists it on top of its HUD by struct update.
pub const fn panels<G: Chrome + LayerPieces>() -> Component<G> {
    Component { install: install_panels::<G>, ..monaka_stereo::hud_panels::hud_panels() }
}

fn install_panels<G: Chrome + LayerPieces>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    GUI.load(Acquire).then_some(()).or_missing("no gui tree to read the pieces from")?;
    monaka_stereo::hud_panels::install_layer::<G>(start)?;
    gui::configure(start.options.switch("probe_gui", false), G::FACTS.pieces, G::showing);
    Ok(())
}
