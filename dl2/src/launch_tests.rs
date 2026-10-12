//! What the launcher makes of this game's manifest: its options file, the viewer's arguments and
//! the notes (`monaka_launch::games::plan`).

use crate::manifest::MANIFESTS;
use monaka_launch::games::{EyeSize, Kind, Values, defaults_are_options, eye_size, options_file, plan, values_of, viewer_args_of};

#[test]
fn defaults_alone_make_a_plan() {
    defaults_are_options(MANIFESTS);
}

#[test]
fn defaults_write_the_manifests_values() {
    let dl2 = &MANIFESTS[0];
    assert_eq!(
        options_file(dl2, &Values::default()),
        "mode=framegen\naim=hand_rig\nlock_pitch=0\nfinger_tracking=1\nfinger_binary=0\nroom_scale=1\ndisable_view_bobbing=1\ndisable_zombie_grabs=1\ncontroller_pad=1\nhud_scale=0.55\nworld_hud=1\nworld_markers=1\nother_builds=0"
    );
    assert_eq!(plan(&Values::default(), dl2, None).licences, ["DyingLight2VR-LICENSE.txt"]);
}

#[test]
fn depth_stereo_pushes_the_hand_panels_out() {
    // Depth stereo pushes the hand panels out to where it shows the hands.
    assert_eq!(viewer_args_of(&MANIFESTS[0], &["aim=head", "mode=depth"]), ["--panel-nearest", "0.5"]);
}

#[test]
fn modes_and_render_sizes() {
    let dl2 = &MANIFESTS[0];
    // The window offers Smooth, Sharp and Fast; alternate-eye and `auto` are command-line only.
    let Kind::Choice { choices, window, .. } = dl2.option("mode").unwrap().kind else { panic!() };
    assert_eq!(choices[window..].iter().map(|(id, _)| *id).collect::<Vec<_>>(), ["alternate", "auto"]);
    let text = options_file(dl2, &values_of(dl2, &["mode=same", "render_size=game", "aim=off", "world_hud=off"]));
    assert!(text.starts_with("mode=same\naim=off\n") && text.contains("\nworld_hud=0\n") && !text.contains("finger_tracking"), "{text}");
    assert_eq!(eye_size(&values_of(dl2, &["render_size=game"]), dl2), EyeSize::GameOwn);
    assert_eq!(eye_size(&values_of(dl2, &["render_size=2160x2160"]), dl2), EyeSize::Fixed(2160, 2160));
}

#[test]
fn auto_is_written_as_asked() {
    // `auto` is Smooth: the producer reads it so (its config), the launcher settles nothing.
    let dl2 = &MANIFESTS[0];
    assert!(options_file(dl2, &values_of(dl2, &["mode=auto"])).contains("mode=auto"));
}
