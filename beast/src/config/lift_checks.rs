//! The stereo method and frame generation against The Beast's old rendering modes (`mode=`), which
//! they replaced: each old mode is a method and a generator that run the same components.

use super::*;

/// What each old mode ran: frame warping, its in-between pairs, per-eye frame generation, FSR frame
/// generation loaded.
fn ran_before(mode: &str) -> (bool, bool, bool, bool) {
    match mode {
        "fast" => (true, false, false, false),
        "smooth" => (true, true, false, true),
        "framegen" => (false, false, true, true),
        _ => (false, false, false, false),
    }
}

/// What runs now, by the components' conditions in `lib.rs`.
fn ran_now(options: &Options) -> (bool, bool, bool, bool) {
    let (afw, framegen) = (method(options) == StereoMethod::Afw, framegen_on(options));
    (afw, afw && framegen, !afw && framegen, framegen)
}

#[test]
fn each_old_mode_is_a_method_and_a_generator_that_run_the_same() {
    for (old, new) in [("standard", "stereo=aer\nframegen=off"), ("fast", "stereo=afw\nframegen=off"), ("smooth", "stereo=afw\nframegen=fsr"), ("framegen", "stereo=aer\nframegen=fsr")] {
        assert_eq!(ran_now(&Options::parse(new)), ran_before(old), "mode={old} is {new}");
    }
    assert_eq!(ran_now(&Options::default()), ran_before("standard"), "the default");
}
