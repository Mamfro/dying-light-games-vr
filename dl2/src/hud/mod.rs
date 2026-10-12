//! The HUD as a layer, its pieces on the hands (`eng_chr::feature`'s ready-made components; the
//! lay-over over each eye is in `stereo::render12`), and its world markers (objectives, waypoints)
//! at their targets' distance ([`markers`]).

pub(crate) mod markers;

use crate::{Dl2, stereo};
use eng_chr::feature::gamedll;
use monaka_core::manifest::{HUD_SIZE, Kind, Opt, Shown, opt};
use monaka_producer::game::{Component, Lack, Missing, On, Start};

pub const HUD: Component<Dl2> = Component { options: &[HUD_SIZE.default_number(0.55)], needs: &[&stereo::EYES], ..eng_chr::feature::hud() };
pub const PANELS: Component<Dl2> = Component { needs: &[&HUD], ..eng_chr::feature::panels() };

const WORLD_MARKERS: Opt =
    opt("world_markers", "Markers in the world", "Objective and waypoint markers sit at their targets' distance instead of in the flat HUD.", Kind::Switch { default: true }).on(Shown::Hud);

/// The HUD's world markers (objectives, waypoints) drawn at their targets' depth with the dynamic
/// HUD ([`markers`]: their world points each tick).
pub const MARKERS: Component<Dl2> = Component {
    name: "markers",
    options: &[WORLD_MARKERS],
    needs: &[&PANELS],
    on: |run| On::when(run.shared.hud.markers, "markers stay in the flat HUD"),
    install: install_markers,
    arm: || markers::enable(true),
    quiet: || markers::enable(false),
    release: markers::report,
    ..Component::NONE
};

fn install_markers(start: &mut Start<'_, Dl2>) -> Result<(), Lack> {
    // SAFETY: the game DLL's hash was checked.
    unsafe { markers::install(start.hooks, gamedll(start)?) }.missing()
}
