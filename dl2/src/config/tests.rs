use super::*;

#[test]
fn methods_and_defaults() {
    let config = Config::default();
    assert!(config.framegen && !config.depth_stereo && !config.alternate_eye, "Standard (Same-frame) with FSR frame generation, as the manifest says");
    assert!(config.aim.head && config.aim.hand && config.aim.rig && config.hud.world && config.hud.markers);
    assert_eq!(config.hud.scale, 0.55);
    assert_eq!(config.once_skip, ONCE_SKIP_DEFAULT);
    let read = |text: &str| Config::from_options(&Options::parse(text), &MANIFESTS[0]);
    let depth = read("stereo=depth\nhud_scale=0.4\naim=off\nlock_pitch=1\ndebug_flags=5\nonce_skip=3");
    assert!(depth.depth_stereo && !depth.framegen && !depth.aim.head);
    assert_eq!((depth.hud.scale, depth.debug, depth.once_skip), (0.4, 5, 3));
    assert!(depth.has(debug::MONO) && !depth.has(debug::SHOW_LEFT));
    let same = read("stereo=same\nframegen=off\nsynthetic_yaw=30\nsynthetic_pitch=-10");
    assert!(!same.framegen && !same.depth_stereo && !same.alternate_eye);
    assert_eq!(same.synthetic, Some((30.0, -10.0)));
    assert!(read("stereo=aer").alternate_eye);
}

#[test]
fn probes_are_research() {
    use crate::research::Options as Probes;
    let none = Probes::from_options(&Options::default());
    assert!(!none.hands && !none.markers && !none.cameras && !none.aim_log);
    let all = Probes::from_options(&Options::parse("probe_hands=1\nprobe_markers=1\nprobe_cameras=1\ndebug_flags=16389"));
    assert!(all.hands && all.markers && all.cameras && all.aim_log);
    assert!(!Probes::from_options(&Options::parse("debug_flags=5")).aim_log);
}
