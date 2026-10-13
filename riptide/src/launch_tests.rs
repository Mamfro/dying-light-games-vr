//! What the launcher makes of this game's manifest: its options file, the viewer's arguments and
//! the notes (`monaka_launch::games::plan`).

use crate::manifest::MANIFESTS;
use monaka_launch::games::{Values, defaults_are_options, plan, viewer_args_of as rings};

#[test]
fn defaults_alone_make_a_plan() {
    defaults_are_options(MANIFESTS);
}

#[test]
fn defaults_write_the_manifests_values() {
    let riptide = &MANIFESTS[0];
    assert_eq!(
        plan(&Values::default(), riptide, Some((2880, 2880))).files[0].1,
        "stereo=afw\nhead_position=lean\nlatency=2\neye_separation=0.064\nhud_scale=0.36\nhud_distance=2\naim=head\ncontroller_pad=1\neye_size=2880x2880"
    );
}

#[test]
fn no_rings_without_a_hand_rig() {
    let riptide = &MANIFESTS[0];
    assert!(rings(riptide, &["aim=head"]).is_empty() && rings(riptide, &["aim=off"]).is_empty());
    assert_eq!(rings(riptide, &["aim=controller"]), ["--reticle", "right"]);
}
