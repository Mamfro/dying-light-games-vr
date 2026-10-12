//! The stereo method and frame generation against DL2's old rendering modes (`mode=`), which they
//! replaced: each old mode is a method and a generator that read the same.

use super::*;

/// What each old mode read: alternate eyes, depth stereo, frame generation (`auto` was frame
/// generation, and so was any other value).
fn read_before(mode: &str) -> (bool, bool, bool) {
    match mode {
        "same" => (false, false, false),
        "alternate" => (true, false, false),
        "depth" => (false, true, false),
        _ => (false, false, true),
    }
}

#[test]
fn each_old_mode_is_a_method_and_a_generator_that_read_the_same() {
    let read = |text: &str| {
        let config = Config::from_options(&Options::parse(text), &MANIFESTS[0]);
        (config.alternate_eye, config.depth_stereo, config.framegen)
    };
    for (old, new) in [("framegen", "stereo=same\nframegen=fsr"), ("same", "stereo=same\nframegen=off"), ("alternate", "stereo=aer\nframegen=off"), ("depth", "stereo=depth\nframegen=off"), ("auto", "")] {
        assert_eq!(read(new), read_before(old), "mode={old} is {new:?}");
    }
    // Frame generation is DL2's on the same-frame pairs only: with another method it stays off.
    assert_eq!(read("stereo=aer\nframegen=fsr"), read_before("alternate"));
    assert_eq!(read("stereo=depth\nframegen=fsr"), read_before("depth"));
}
