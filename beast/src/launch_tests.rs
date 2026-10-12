//! What the launcher makes of this game's manifest: its options file, the viewer's arguments and
//! the notes (`monaka_launch::games::plan`).

use crate::manifest::MANIFESTS;
use monaka_launch::games::{EyeSize, Shown, Values, defaults_are_options, eye_size, options_file, plan, values_of, viewer_args_of};

/// The viewer's arguments without how the wheels are picked (checked on its own).
fn rings(args: &[&str]) -> Vec<String> {
    let mut v = viewer_args_of(&MANIFESTS[0], args);
    if let Some(at) = v.iter().position(|a| a == "--menu-pick") {
        v.drain(at..at + 2);
    }
    v
}

#[test]
fn defaults_alone_make_a_plan() {
    defaults_are_options(MANIFESTS);
}

#[test]
fn defaults_write_the_manifests_values() {
    let beast = &MANIFESTS[0];
    assert_eq!(
        options_file(beast, &Values::default()),
        "stereo=aer\nhead_position=lean\nlatency=2\neye_separation=0.064\nframegen=off\nheadset_depth=1\naim=hand_rig\nlock_pitch=1\nfinger_tracking=1\nfinger_binary=0\nroom_scale=1\ndisable_view_bobbing=1\ndisable_zombie_grabs=1\ncontroller_pad=1\nupscale_per_eye=0\nfoveated_rendering=1\nfoveated_inner=15\nfoveated_middle=30\nfoveated_outer=2x4\nhud_scale=0.5\nhud_distance=2\nworld_hud=1\nother_builds=0"
    );
    let plan = plan(&Values::default(), beast, Some((2160, 2160)));
    assert!(plan.files[0].1.ends_with("\neye_size=2160x2160"));
    assert_eq!(plan.hints.len(), 1);
    assert_eq!(rings(&["aim=hand_rig"]), ["--reticle", "right", "--reticle", "left", "--reticle-from", "palm"]);
}

#[test]
fn takes_its_own_eye_size() {
    let beast = &MANIFESTS[0];
    assert_eq!(eye_size(&values_of(beast, &["render_size=game"]), beast), EyeSize::GameOwn);
    assert_eq!(eye_size(&values_of(beast, &["render_size=game", "eye_size=2560x2560"]), beast), EyeSize::Fixed(2560, 2560));
    // Its note, from the manifest, with the size settled.
    let hints = plan(&Values::default(), beast, Some((2160, 2160))).hints;
    assert!(hints.len() == 1 && hints[0].contains("2160x2160"), "{hints:?}");
    assert!(Values::default().set_checked(beast, "eye_size", "100x100").is_err());
    let text = options_file(beast, &values_of(beast, &["aim=hand_rig", "stereo=afw", "framegen=fsr", "latency=3"]));
    assert!(text.starts_with("stereo=afw\nhead_position=lean\nlatency=3\neye_separation=0.064\nframegen=fsr\n"), "{text}");
    // The window offers the shared settings and none of the command line's.
    let shown: Vec<&str> = beast.options.iter().filter(|o| o.shown != Shown::Line).map(|o| o.id).collect();
    assert_eq!(
        shown,
        ["render_size", "stereo", "framegen", "headset_depth", "aim", "aim_hand", "finger_tracking", "finger_binary", "room_scale", "disable_view_bobbing", "disable_zombie_grabs", "controller_pad", "controller_proxy", "upscale_per_eye", "foveated_rendering", "hud_scale", "hud_distance", "world_hud", "other_builds"]
    );
}

#[test]
fn the_wheels_open_by_reach() {
    // The tools and weapons wheels stay open while the d-pad is held: the viewer holds them, and
    // opens them for a hand reached over a shoulder (its own default: nothing passed for it). No
    // setting in the window.
    let beast = &MANIFESTS[0];
    let all = plan(&Values::default(), beast, None).viewer_args;
    assert!(all.windows(2).any(|w| w == ["--dpad-hold", "left,right"]));
    // Its wheels pick with the right stick: the reaching hand's flick goes there.
    assert!(all.windows(2).any(|w| w == ["--menu-pick", "stick"]));
    assert!(!all.iter().any(|a| a.starts_with("--menu-reach")));
    assert_eq!(beast.option("dpad_hold").map(|o| o.shown), Some(Shown::Line));
    assert!(beast.option("menu_reach").is_none());
}

#[test]
fn the_stereo_method_and_frame_generation() {
    // AER and no frame generation by default; AFW warps each frame into the other eye; the methods
    // The Beast does not have are not offered.
    let beast = &MANIFESTS[0];
    assert_eq!(beast.choice_default("stereo"), Some("aer"));
    assert_eq!(beast.choice_default("framegen"), Some("off"));
    for method in ["aer", "afw"] {
        assert!(Values::default().set_checked(beast, "stereo", method).is_ok(), "{method}");
    }
    assert!(Values::default().set_checked(beast, "stereo", "same").is_err());
    assert!(Values::default().set_checked(beast, "stereo", "off").is_ok(), "stereo off, from the command line");
    let text = options_file(beast, &values_of(beast, &["stereo=afw", "framegen=fsr"]));
    assert!(text.starts_with("stereo=afw\n") && text.contains("\nframegen=fsr\n"), "{text}");
}
