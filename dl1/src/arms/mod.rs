//! The hand rig and what rides on it: the arms on the controllers over the shared Chrome rig
//! ([`hands`]), the hands in the world ([`hand_world`]) and what they do there: physical melee
//! ([`melee`]), throwing tools ([`throwing`]), the bow ([`bow`]), guns fired from the barrel
//! ([`gun`]; both through [`projectile`]), the grappling hook ([`grapple`]) and lockpicking
//! ([`lockpick`]); whether the player climbs ([`climbing`]), and the player's walk body with
//! room-scale following on it ([`walk`], [`roomscale`]).

pub(crate) mod bow;
pub(crate) mod climbing;
pub(crate) mod grapple;
pub(crate) mod gun;
pub(crate) mod hand_world;
pub(crate) mod hands;
pub(crate) mod lockpick;
pub(crate) mod melee;
pub(crate) mod projectile;
pub(crate) mod roomscale;
pub(crate) mod throwing;
pub(crate) mod walk;

use crate::config::Config;
use crate::{Dl1, look};
use monaka_core::feature::Feature;
use monaka_hook::Hooks;
use monaka_hook::module::Module;
use monaka_producer::game::{Component, Lack, On, Start, Switch};

/// The arms on the controllers at the arms visual's camera-target callback ([`hands`]), a swing of
/// a held weapon or a fist attacking; what the hands do with the world's objects from where the
/// rig puts them ([`hand_world`]).
pub const ARMS: Component<Dl1> = Component {
    name: "arms",
    features: &[Feature::HandRig],
    needs: &[&look::LOOK],
    on: |run| On::when(run.shared.aim.rig, "aim is not motion controls"),
    install: install_arms,
    release: hand_world::report,
    ..Component::NONE
};

fn install_arms(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    hook_arms(start.hooks, &start.build.engine, &start.build.gamedll)?;
    hand_world::resolve(&start.build.engine)?;
    Ok(())
}

/// The arms visual's camera-target callback (the rig, and its probes).
pub(crate) fn hook_arms(hooks: &mut Hooks, engine_module: &Module, gamedll: &Module) -> Result<(), Lack> {
    hands::resolve(engine_module)?;
    hooks.site(&hands::CAMERA_TARGET_ORIGINAL, gamedll, &crate::engine::FPP_CAMERA_TARGET, hands::camera_target)?;
    Ok(())
}

/// A free hand's fingers on the tracked fingers (`eng_chr::fingers` at the arms' pose), read from
/// the controllers' finger block.
pub const FINGERS: Component<Dl1> = Component {
    needs: &[&ARMS],
    needs_channel: true,
    install: install_fingers,
    release: monaka_arms::controllers::close,
    ..monaka_arms::feature::fingers()
};

fn install_fingers(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    monaka_arms::controllers::open(start.channel()?);
    monaka_arms::fingers::set_binary(start.shared.fingers.binary);
    Ok(())
}

/// Physical melee ([`melee`]): a swing hits where the weapon in the hand is; with `melee_on_reach`
/// a weapon's swing attacks only once the blade is about to reach something, and with
/// `back_is_blunt` a cutting weapon's hit not led by its edge lands blunt.
pub const MELEE: Component<Dl1> = Component {
    name: "melee",
    features: &[Feature::Melee],
    needs: &[&ARMS],
    on: |run| On::when(Config::from_options(run.options, run.manifest).physical_melee.is_some(), "physical melee is off"),
    install: install_melee,
    ..Component::NONE
};

fn install_melee(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = crate::config(start);
    let Some(physical) = config.physical_melee else { return Ok(()) };
    melee::install(start.hooks, &start.build.gamedll, &start.build.engine, physical, config.melee_on_reach, config.back_is_blunt)?;
    Ok(())
}

/// Tools thrown with the left hand's own throw ([`throwing`]; `throw_by_hand=0`: the game's throw
/// along the view).
pub const THROW: Component<Dl1> = Component {
    name: "throw",
    features: &[Feature::Throw],
    needs: &[&ARMS],
    switch: Some(Switch::new("throw_by_hand", true, "the game's throw")),
    install: |start| Ok(throwing::install(start.hooks, &start.build.gamedll, &start.build.engine)?),
    release: throwing::report,
    ..Component::NONE
};

/// The bow drawn by pulling the string hand back, loosed along the hands ([`bow`]; `bow_by_hand=0`:
/// the game's shot along the view).
pub const BOW: Component<Dl1> = Component {
    name: "bow",
    features: &[Feature::Bow],
    needs: &[&ARMS],
    switch: Some(Switch::new("bow_by_hand", true, "the game's bow")),
    install: |start| Ok(bow::install(start.hooks, &start.build.gamedll)?),
    release: bow::report,
    ..Component::NONE
};

/// The grappling hook goes where the left controller points ([`grapple`]; `grapple_by_hand=0`:
/// where the character looks).
pub const GRAPPLE: Component<Dl1> = Component {
    name: "grapple",
    needs: &[&ARMS],
    switch: Some(Switch::new("grapple_by_hand", true, "the game's grapple")),
    install: |start| Ok(grapple::install(start.hooks, &start.build.gamedll)?),
    release: grapple::report,
    ..Component::NONE
};

/// Lockpicking by twisting the hands ([`lockpick`]; `lockpick_by_hand=0`: the sticks).
pub const LOCKPICK: Component<Dl1> = Component {
    name: "lockpick",
    features: &[Feature::Lockpick],
    needs: &[&ARMS],
    switch: Some(Switch::new("lockpick_by_hand", true, "the game's lockpick")),
    install: |start| Ok(lockpick::install(start.hooks, &start.build.gamedll)?),
    release: lockpick::report,
    ..Component::NONE
};

/// A gun's shots leave its muzzle along its barrel ([`gun`]; `shoot_from_barrel=0`: from the eye
/// along the aim).
pub const GUN: Component<Dl1> = Component {
    name: "gun",
    needs: &[&ARMS],
    switch: Some(Switch::new("shoot_from_barrel", true, "shots from the eye")),
    install: |start| Ok(gun::install(start.hooks, &start.build.gamedll)?),
    release: gun::report,
    ..Component::NONE
};

/// The character follows the player's real steps (`monaka_arms::roomscale` at DL1's own walk body,
/// [`walk`]), with the hand rig; the walk body is put back as the game had it.
pub const ROOM_SCALE: Component<Dl1> = Component {
    name: "room scale",
    features: &[Feature::RoomScale],
    options: &[monaka_core::manifest::ROOM_SCALE],
    needs: &[&ARMS],
    on: |run| On::when(monaka_arms::roomscale::Settings::from_options(run.options, run.shared).is_some(), "room-scale movement is off"),
    install: install_room_scale,
    quiet: monaka_arms::roomscale::stop,
    give_back: walk::restore,
    ..Component::NONE
};

fn install_room_scale(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let Some(settings) = monaka_arms::roomscale::Settings::from_options(start.options, start.shared) else { return Ok(()) };
    walk::install(start.hooks, &start.build.engine)?;
    monaka_arms::roomscale::install(settings);
    Ok(())
}
