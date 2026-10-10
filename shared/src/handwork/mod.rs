//! What the hands do with the game's own actions, with the hand rig (Dying Light 2 and The Beast):
//! throwing a throwable ([`throwing`]) or the melee weapon ([`weapon_throw`]) by hand, the bow
//! drawn and aimed by the hands ([`bow`], through the aim part [`aim`]), reloading with the left
//! hand at the gun ([`reload`]) and lockpicking by twisting the wrists ([`lockpick`]). Each game
//! crate supplies its build's addresses ([`Builds`]) and its options ([`Options`]).

pub mod aim;
pub mod bow;
pub mod lockpick;
pub mod reload;
pub mod throwing;
pub mod weapon_throw;

use monaka_hook::Hooks;
use monaka_hook::module::Module;
use monaka_producer::Rejection;

/// What the hands do, for a run.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Options {
    /// Throwables and the melee weapon thrown by hand.
    pub throw: bool,
    /// The bow drawn and aimed by the hands.
    pub bow: bool,
    /// The reload runs with the left hand at the gun.
    pub reload: bool,
    /// Lockpicking by twisting the wrists.
    pub lockpick: bool,
}

/// One build's addresses for each part (none where the build has not been read for it).
#[derive(Clone, Copy, Debug)]
pub struct Builds {
    pub throw: Option<throwing::Build>,
    pub weapon_throw: Option<weapon_throw::Build>,
    pub aim: aim::Build,
    pub bow: Option<bow::Build>,
    pub reload: Option<reload::Build>,
    pub lockpick: Option<lockpick::Build>,
}

/// Installs the parts `options` asks for that `builds` has; a part that cannot hook is logged and
/// left out, the rest go on.
pub fn install(hooks: &mut Hooks, gamedll: &Module, builds: &Builds, options: Options) {
    let part = |name: &str, wanted: bool, install: Option<Result<(), Rejection>>| {
        if !wanted {
            return;
        }
        match install {
            Some(Ok(())) => monaka_producer::log!("{name}: on"),
            Some(Err(why)) => monaka_producer::log!("{name}: left out ({why:?})"),
            None => monaka_producer::log!("{name}: not read for this build"),
        }
    };
    part("throw by hand", options.throw, options.throw.then(|| builds.throw.map(|b| throwing::install(hooks, gamedll, b))).flatten());
    part("weapon thrown by hand", options.throw, options.throw.then(|| builds.weapon_throw.map(|b| weapon_throw::install(hooks, gamedll, b))).flatten());
    part("bow by hand", options.bow, options.bow.then(|| builds.bow.map(|b| bow::install(hooks, gamedll, b, builds.aim))).flatten());
    part("reload by hand", options.reload, options.reload.then(|| builds.reload.map(|b| reload::install(hooks, gamedll, b))).flatten());
    part("lockpick by hand", options.lockpick, options.lockpick.then(|| builds.lockpick.map(|b| lockpick::install(hooks, gamedll, b))).flatten());
}

/// The parts' tallies for the end-of-run report.
pub fn report() {
    throwing::report();
    weapon_throw::report();
    bow::report();
    reload::report();
    lockpick::report();
}
