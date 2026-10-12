//! The pose report the present loop now reads from the eye site (`EyeSite for Beast`, through
//! `monaka_stereo::eye_site::loop_report`) against The Beast's own report it replaced.

use super::*;

/// The Beast's report as it was: the eye distance used, each eye's field of view as rendered.
fn report_before(used: &mut HeadPose) {
    if let Some(shared) = shared() {
        used.ipd = shared.separation_for(used);
    }
    let pose = *used;
    for (eye, fov) in used.fov.iter_mut().enumerate() {
        if let Some(frustum) = eye_frustum(&pose, eye) {
            *fov = frustum.fov();
        }
    }
}

#[test]
fn each_eye_goes_out_as_it_went_out_before() {
    let poses = [
        HeadPose { valid: 1, ipd: 0.064, fov: [[-0.87, 0.75, 0.82, -0.86], [-0.75, 0.87, 0.82, -0.86]], ..HeadPose::default() },
        HeadPose { valid: 1, ipd: 0.2, fov: [[0.0; 4], [-0.75, 0.87, 0.82, -0.86]], ..HeadPose::default() },
        HeadPose::default(),
    ];
    let shared = Shared::from_options(&monaka_core::options::Options::parse(""), &crate::manifest::MANIFESTS[0]);
    for installed in [None, Some(shared)] {
        *SHARED.locked() = installed;
        for headset in [false, true] {
            HEADSET_FOV.store(headset, Release);
            for frusta in [0, 3] {
                FRUSTA.store(frusta, Relaxed);
                for scale in [[0.0, 0.0], [1.2, 1.8]] {
                    GAME_SCALE[0].store(scale[0]);
                    GAME_SCALE[1].store(scale[1]);
                    for pose in poses {
                        let (mut before, mut now) = (pose, pose);
                        report_before(&mut before);
                        monaka_stereo::eye_site::report::<crate::Beast>(&mut now);
                        assert_eq!(now, before, "{installed:?} {headset} {frusta} {scale:?}");
                    }
                }
            }
        }
    }
    *SHARED.locked() = None;
    HEADSET_FOV.store(false, Release);
    FRUSTA.store(0, Relaxed);
}
