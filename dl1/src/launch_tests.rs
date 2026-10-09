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
    let dl1 = &MANIFESTS[0];
    assert_eq!(
        file(dl1, &Values::default(), Some((2880, 2880))),
        "aim=hand_rig\nworld_hud=1\nhud_scale=0.36\nhud_distance=2\nfinger_tracking=1\nroom_scale=1\nfinger_binary=0\nfsr=0\neye_separation=0.064\nother_builds=0\neye_size=2880x2880"
    );
    assert_eq!(eye_size(&Values::default(), dl1), EyeSize::Headset);
}

#[test]
fn rings_follow_the_aim() {
    let dl1 = &MANIFESTS[0];
    assert_eq!(rings(dl1, &["aim=hand_rig"]), ["--reticle", "right", "--reticle", "left", "--reticle-from", "palm"]);
    assert_eq!(rings(dl1, &["aim=controller"]), ["--reticle", "right", "--reticle", "left"]);
    assert_eq!(rings(dl1, &["aim=controller", "aim_hand=left"]), ["--reticle", "left"]);
    assert!(rings(dl1, &["aim=head"]).is_empty() && rings(dl1, &["aim=off"]).is_empty());
    // The tools and weapons stay open while the d-pad is held: the viewer holds them for a
    // controller without one, unless the player turns that off.
    let all = |args: &[&str]| plan(&parsed(dl1, args), dl1, None).viewer_args;
    assert!(all(&["aim=head"]).ends_with(&["--dpad-hold".to_owned(), "left,right".to_owned()]));
    assert!(!all(&["aim=head", "dpad_hold=off"]).contains(&"--dpad-hold".to_owned()));
    // Menus by reach: on by default (the viewer's own default), so nothing passed; the other
    // hand's stick or off are passed; with the held menus off, it does not apply.
    assert!(!all(&["aim=head"]).contains(&"--menu-reach".to_owned()));
    assert!(all(&["aim=head", "menu_reach=other"]).ends_with(&["--menu-reach".to_owned(), "other".to_owned()]));
    assert!(!all(&["aim=head", "menu_reach=off", "dpad_hold=off"]).contains(&"--menu-reach".to_owned()));
    // The reach depth: the default passes nothing, another is passed in cm.
    assert!(!all(&["aim=head"]).contains(&"--menu-reach-depth".to_owned()));
    let deeper = all(&["aim=head", "menu_reach_depth=25"]);
    let at = deeper.iter().position(|a| a == "--menu-reach-depth").unwrap();
    assert_eq!(deeper[at + 1], "25");
    // The controller test passes its choice; off passes nothing.
    assert!(all(&["aim=head", "controller_proxy=index"]).ends_with(&["--act-like".to_owned(), "index".to_owned()]));
    assert!(!all(&["aim=head"]).contains(&"--act-like".to_owned()));
}

#[test]
fn options_apply_only_where_they_mean_something() {
    let dl1 = &MANIFESTS[0];
    // The aiming hand only for a pointing controller; the fingers only with the rig.
    let text = file(dl1, &parsed(dl1, &["aim=hand_rig", "aim_hand=left", "finger_binary=on"]), None);
    assert!(!text.contains("aim_hand") && text.contains("finger_binary=1"), "{text}");
    let text = file(dl1, &parsed(dl1, &["aim=controller", "aim_hand=left", "finger_binary=on"]), None);
    assert!(text.contains("aim_hand=left") && !text.contains("finger"), "{text}");
    // Extra pairs go last, junk dropped.
    let text = file(dl1, &parsed(dl1, &["extra=probe_rig=1 junk hand_hold=0.01,0,-0.02"]), None);
    assert!(text.ends_with("eye_separation=0.064\nother_builds=0\nprobe_rig=1\nhand_hold=0.01,0,-0.02"), "{text}");
    // FSR, as the manifest declares: with an eye size and AMD's DLLs from Dying Light 2 only;
    // without either it is written off and the player told why.
    let fsr = parsed(dl1, &["fsr=on", "fsr_scale=2"]);
    let dlls = |_: &SteamFiles| Some(std::path::PathBuf::from(r"D:\Games\Dying Light 2\ph\work\bin\x64"));
    let found = plan_with(&fsr, dl1, Some((2880, 2880)), dlls);
    let text = &found.files[0].1;
    assert!(text.contains("fsr=1\nfsr_scale=2\nfsr_sharpness=0.3\n") && text.ends_with(r"fsr_dlls=D:\Games\Dying Light 2\ph\work\bin\x64") && found.hints.is_empty(), "{text}");
    let missing = plan_with(&fsr, dl1, Some((2880, 2880)), |_| None);
    assert!(missing.files[0].1.contains("fsr=0") && !missing.files[0].1.contains("fsr_scale"), "{}", missing.files[0].1);
    assert!(missing.hints.len() == 1 && missing.hints[0].contains("FidelityFX"), "{:?}", missing.hints);
    let without = plan_with(&fsr, dl1, None, dlls);
    assert!(without.files[0].1.contains("fsr=0") && without.hints.len() == 1 && without.hints[0].contains("headset resolution"), "{:?}", without.hints);
}
