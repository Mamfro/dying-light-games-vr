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

/// The viewer's arguments without Dying Light's held d-pad menus (checked on their own).
#[allow(dead_code)]
fn rings(game: &Manifest, args: &[&str]) -> Vec<String> {
    let mut v = plan(&parsed(game, args), game, None).viewer_args;
    if let Some(at) = v.iter().position(|a| a == "--dpad-hold") {
        v.drain(at..at + 2);
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
    let dl2 = &MANIFESTS[0];
    assert_eq!(file(dl2, &Values::default(), None), "aim=hand_rig\nworld_hud=1\nworld_markers=1\nhud_scale=0.55\nfinger_tracking=1\nroom_scale=1\nfinger_binary=0\nmode=framegen\nlock_pitch=0\nother_builds=0");
    assert_eq!(plan(&Values::default(), dl2, None).licences, ["DyingLight2VR-LICENSE.txt"]);
}

#[test]
fn depth_stereo_pushes_the_hand_panels_out() {
    // Depth stereo pushes the hand panels out to where it shows the hands.
    assert_eq!(rings(&MANIFESTS[0], &["aim=head", "mode=depth"]), ["--panel-nearest", "0.5"]);
}

#[test]
fn modes_and_render_sizes() {
    let dl2 = &MANIFESTS[0];
    // The window offers Smooth, Sharp and Fast; alternate-eye and `auto` are command-line only.
    let Kind::Choice { choices, window, .. } = dl2.option("mode").unwrap().kind else { panic!() };
    assert_eq!(choices[window..].iter().map(|(id, _)| *id).collect::<Vec<_>>(), ["alternate", "auto"]);
    assert_eq!(file(dl2, &parsed(dl2, &["mode=same", "render_size=game", "aim=off", "world_hud=off"]), None), "aim=off\nworld_hud=0\nworld_markers=1\nhud_scale=0.55\nmode=same\nlock_pitch=0\nother_builds=0");
    assert_eq!(eye_size(&parsed(dl2, &["render_size=game"]), dl2), EyeSize::GameOwn);
    assert_eq!(eye_size(&parsed(dl2, &["render_size=2160x2160"]), dl2), EyeSize::Fixed(2160, 2160));
}

#[test]
fn auto_is_written_as_asked() {
    // `auto` is Smooth: the producer reads it so (its config), the launcher settles nothing.
    let dl2 = &MANIFESTS[0];
    assert!(file(dl2, &parsed(dl2, &["mode=auto"]), None).contains("mode=auto"));
}
