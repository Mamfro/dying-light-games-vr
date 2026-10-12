//! DL1's eye site as the present loop now reads it against DL1's own present steps it replaced.

use super::*;
use crate::manifest::MANIFESTS;
use monaka_core::options::Options;

/// DL1's pose report as it was: the eye distance used; without the headset's projection the
/// engine's own frustum for both eyes, with the symmetric experiment each eye's rendered one.
fn report_before(config: &Config, game: Option<Frustum>, used: &mut HeadPose) {
    used.ipd = config.separation_for(used);
    if !config.headset_fov {
        if let Some(engine) = game {
            used.fov = [engine.fov(); 2];
        }
    } else if config.symmetric_projection {
        let rendered = [config.rendered_frustum(used, 0), config.rendered_frustum(used, 1)];
        for (fov, frustum) in used.fov.iter_mut().zip(rendered) {
            if let Some(frustum) = frustum {
                *fov = frustum.fov();
            }
        }
    }
}

/// The loop's report (`monaka_stereo::eye_site::report`) over DL1's eye site, for `config`.
fn report_now(config: &Config, game: Option<Frustum>, used: &mut HeadPose) {
    used.ipd = config.separation_for(used);
    let drawn = *used;
    for (eye, fov) in used.fov.iter_mut().enumerate() {
        if let Some(frustum) = drawn_frustum(config, game, &drawn, eye) {
            *fov = frustum.fov();
        }
    }
}

#[test]
fn each_pair_goes_out_as_dl1_reported_it() {
    let poses = [
        HeadPose { valid: 1, ipd: 0.064, fov: [[-0.87, 0.75, 0.82, -0.86], [-0.75, 0.87, 0.82, -0.86]], ..HeadPose::default() },
        HeadPose { valid: 1, ipd: 0.0, fov: [[0.0; 4], [-0.75, 0.87, 0.82, -0.86]], ..HeadPose::default() },
        HeadPose::default(),
    ];
    let games = [None, Frustum::from_fov([-0.7, 0.7, 0.45, -0.45])];
    for text in ["", "headset_fov=0", "symmetric_projection=1", "test_separation=0.3"] {
        let config = Config::from_options(&Options::parse(text), &MANIFESTS[0]);
        for game in games {
            for pose in poses {
                let (mut before, mut now) = (pose, pose);
                report_before(&config, game, &mut before);
                report_now(&config, game, &mut now);
                assert_eq!(now, before, "{text} {game:?}");
            }
        }
    }
}
