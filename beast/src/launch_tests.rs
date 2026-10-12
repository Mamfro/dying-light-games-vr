//! What the launcher makes of this game's manifest: its options file, the viewer's arguments and
//! the notes (`monaka_launch::games::plan`).

use crate::manifest::MANIFESTS;
#[allow(unused_imports)]
use monaka_launch::games::{EyeSize, Kind, Manifest, Shown, SteamFiles, Values, eye_size, plan, plan_with};

/// What `monaka_play` makes of its `id=value` arguments.
#[allow(dead_code)]
fn parsed(game: &Manifest, args: &[&str]) -> Values {
    let mut values = Values::default();
    for arg in args {
        let (id, value) = arg.split_once('=').unwrap();
        values.set_checked(game, id, value).unwrap_or_else(|why| panic!("{why}"));
    }
    values
}

/// The options file a run writes.
fn file(game: &Manifest, values: &Values, eye: Option<(u32, u32)>) -> String {
    let plan = plan(values, game, eye);
    assert_eq!(plan.files.len(), 1);
    assert_eq!(plan.files[0].0, game.options_file);
    plan.files[0].1.clone()
}

/// The viewer's arguments without Dying Light's held d-pad menus and how they are picked
/// (checked on their own).
#[allow(dead_code)]
fn rings(game: &Manifest, args: &[&str]) -> Vec<String> {
    let mut v = plan(&parsed(game, args), game, None).viewer_args;
    for flag in ["--dpad-hold", "--menu-pick"] {
        if let Some(at) = v.iter().position(|a| a == flag) {
            v.drain(at..at + 2);
        }
    }
    v
}

#[test]
fn defaults_alone_make_a_plan() {
    for game in MANIFESTS {
        let defaults = file(game, &Values::default(), None);
        assert!(defaults.lines().all(|l| l.split_once('=').is_some_and(|(k, _)| game.option(k).is_some())), "{}: {defaults}", game.id);
    }
}

#[test]
fn defaults_write_the_manifests_values() {
    let beast = &MANIFESTS[0];
    assert_eq!(file(beast, &Values::default(), Some((2160, 2160))), "aim=hand_rig\nworld_hud=1\nfinger_tracking=1\nroom_scale=1\ndisable_view_bobbing=1\ndisable_zombie_grabs=1\nfinger_binary=0\nmode=standard\nstereo=1\nlatency=2\neye_separation=0.064\nother_builds=0\neye_size=2160x2160");
    assert_eq!(plan(&Values::default(), beast, Some((2160, 2160))).hints.len(), 1);
    assert_eq!(rings(beast, &["aim=hand_rig"]), ["--reticle", "right", "--reticle", "left", "--reticle-from", "palm"]);
}

#[test]
fn takes_its_own_eye_size() {
    let beast = &MANIFESTS[0];
    assert_eq!(eye_size(&parsed(beast, &["render_size=game"]), beast), EyeSize::GameOwn);
    assert_eq!(eye_size(&parsed(beast, &["render_size=game", "eye_size=2560x2560"]), beast), EyeSize::Fixed(2560, 2560));
    // Its note, from the manifest, with the size settled.
    let hints = plan(&Values::default(), beast, Some((2160, 2160))).hints;
    assert!(hints.len() == 1 && hints[0].contains("2160x2160"), "{hints:?}");
    assert!(Values::default().set_checked(beast, "eye_size", "100x100").is_err());
    let text = file(beast, &parsed(beast, &["aim=hand_rig", "mode=framegen", "latency=3", "stereo=off"]), None);
    assert_eq!(text, "aim=hand_rig\nworld_hud=1\nfinger_tracking=1\nroom_scale=1\ndisable_view_bobbing=1\ndisable_zombie_grabs=1\nfinger_binary=0\nmode=framegen\nstereo=0\nlatency=3\neye_separation=0.064\nother_builds=0");
    // The window offers the shared settings and none of the command line's.
    assert!(beast.options.iter().filter(|o| o.shown != Shown::Line).map(|o| o.id).eq(["aim", "aim_hand", "render_size", "world_hud", "finger_tracking", "room_scale", "disable_view_bobbing", "disable_zombie_grabs", "finger_binary", "mode", "controller_proxy", "other_builds"]));
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
