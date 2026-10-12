//! Providers: frame labels (`FrameLabels`), frame inputs (`FrameInputs`, collected by
//! `crate::INPUTS`) and the HUD layer (`HudLayer`). The labels come from the eye site's pair
//! ([`eye_site`]).
//!
//! What Dying Light 2 provides once for the ready-made features (`monaka_stereo::providers`): each
//! frame's eye from the scene drawing it (`eye_site::RENDER_EYE`) with its pair's head pose, each eye
//! drawn with its own field of view, and each eye's inputs from its Streamline calls for DLSS
//! (the renderer's own per-eye DLSS: copied when tagged, the frame-generation call's at the eye's
//! present), and its HUD layer laid over each rendered eye (`eng_chr::providers::hud_over`).

use crate::providers::eye_site::{self, local};
use crate::providers::hud;
use monaka_stereo::eye_site::EyeSite;
use monaka_stereo::same_frame::PAIRS;
use crate::{Dl2, config};
use monaka_core::camera::Frustum;
use monaka_core::protocol::HeadPose;
use monaka_stereo::driver::Pending;
use monaka_stereo::hudover12::Params;
use monaka_stereo::providers::{CopyAt, FrameInputs, FrameLabels, HudLayer, Sources};

impl FrameLabels for Dl2 {
    fn labelled() -> Option<Pending> {
        eye_site::render_frame().map(|(eye, pose)| Pending { eye, pose })
    }

    /// The eye whose present is being dispatched (on the render thread), with its pair's pose.
    fn presenting() -> Option<Pending> {
        let (eye, pose) = local(|l| (l.dispatch_eye.get(), l.pair_head.get()));
        (1..=2).contains(&eye).then(|| Pending { eye: (eye - 1) as usize, pose })
    }

    fn running() -> bool {
        PAIRS.running()
    }

    fn frustum(pose: &HeadPose, eye: usize) -> Option<Frustum> {
        <Dl2 as EyeSite>::frustum(pose, eye)
    }

    fn separation(pose: &HeadPose) -> Option<f32> {
        <Dl2 as EyeSite>::separation(pose)
    }
}

/// Depth reversed and infinite, its near plane in each frame's projection; the game's units are
/// metres.
impl FrameInputs for Dl2 {
    const SOURCES: Sources = Sources { streamline: Some(CopyAt::Tags), fsr: false };

    fn late_at_present() -> bool {
        !config::debug(config::debug::LATE_INPUTS_AT_TAG)
    }
}

/// The HUD layer over a rendered eye (Standard, AER, and frame generation's real and generated
/// frames), with the dynamic HUD: in that eye's frustum at the HUD's distance and size (the HUD's
/// facts given `eng_chr::providers::hud_over` when the eyes start), the crosshair at the game's aim
/// point. Depth-stereo places it in its centre frame itself (`crate::extras::depth_stereo`): no eye
/// presents there.
impl HudLayer for Dl2 {
    fn over(size: (u32, u32), eye: Option<usize>) -> Option<Params> {
        eng_chr::providers::hud_over::over::<Self>(size, eye).map(|params| Params { aim: hud::aim(size), ..params })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monaka_core::hud::{Placement, Rect};

    /// The rendered eyes' HUD placement as the renderer side worked it out for itself before the
    /// HUD layer was a provider: each eye in its own frustum at the HUD's distance and size, its
    /// disparity from the pair's eye separation, the whole frame for an eye without a usable field
    /// of view.
    fn as_before(size: (u32, u32), pose: &HeadPose, scale: f32, distance: f32) -> [[f32; 4]; 2] {
        let image = Rect::sized(size.0 as f32, size.1 as f32);
        let frustums = [Frustum::from_fov(pose.fov[0]), Frustum::from_fov(pose.fov[1])];
        std::array::from_fn(|eye| match frustums[eye] {
            Some(frustum) => Placement::new(frustum, eye, pose.ipd, distance).rect(image, image.aspect(), scale).array(),
            None => image.array(),
        })
    }

    /// The same through the shared placement and DL2's frame labels, at eye `dispatch`'s present.
    fn shared(size: (u32, u32), pose: HeadPose, dispatch: u32, scale: f32, distance: f32) -> [[f32; 4]; 2] {
        local(|l| {
            l.dispatch_eye.set(dispatch);
            l.pair_head.set(pose);
        });
        let whole = monaka_stereo::providers::whole_frame(size);
        let rects = std::array::from_fn(|eye| eng_chr::providers::hud_over::place::<Dl2>(size, Some(eye), scale, distance).unwrap_or(whole).rect);
        local(|l| l.dispatch_eye.set(0));
        rects
    }

    #[test]
    fn the_rendered_eyes_hud_goes_where_it_went() {
        let pose = HeadPose { valid: 1, ipd: 0.064, fov: [[-0.87, 0.75, 0.82, -0.86], [-0.75, 0.87, 0.82, -0.86]], ..HeadPose::default() };
        let mut one_eye = pose;
        one_eye.fov[1] = [0.0; 4];
        let mut neither = pose;
        neither.fov = [[0.0; 4]; 2];
        // The game's sizes at the headset's eye size and on a monitor; DL2's HUD size and others.
        for size in [(2016, 2240), (1920, 1080)] {
            for (scale, distance) in [(0.55, 1.4), (0.4, 2.0), (1.0, 0.8)] {
                for pose in [pose, one_eye, neither] {
                    let old = as_before(size, &pose, scale, distance);
                    // The left eye's present and the right's.
                    for dispatch in [1, 2] {
                        assert_eq!(shared(size, pose, dispatch, scale, distance), old, "{size:?} {scale} {distance} {:?}", pose.fov);
                    }
                }
            }
        }
    }

    #[test]
    fn the_eye_presenting_is_the_one_dispatched() {
        let pose = HeadPose { valid: 1, ipd: 0.064, fov: [[-0.87, 0.75, 0.82, -0.86], [-0.75, 0.87, 0.82, -0.86]], ..HeadPose::default() };
        let size = (2016, 2240);
        for (dispatch, eye) in [(1, 0), (2, 1)] {
            local(|l| {
                l.dispatch_eye.set(dispatch);
                l.pair_head.set(pose);
            });
            let own = eng_chr::providers::hud_over::place::<Dl2>(size, None, 0.55, 1.4).map(|p| p.rect);
            let named = eng_chr::providers::hud_over::place::<Dl2>(size, Some(eye), 0.55, 1.4).map(|p| p.rect);
            assert!(own.is_some() && own == named);
        }
        // Depth stereo's centre frame (3) and no present: no eye to place it in.
        for dispatch in [0, 3] {
            local(|l| l.dispatch_eye.set(dispatch));
            assert!(Dl2::presenting().is_none());
        }
    }

    #[test]
    fn the_aim_point_is_the_pairs_in_pixels() {
        let size = (2016, 2240);
        local(|l| l.pair_aim.set(None));
        assert_eq!(hud::aim(size), [0.0, 0.0]);
        local(|l| l.pair_aim.set(Some([0.25, -0.5])));
        assert_eq!(hud::aim(size), [0.25 * 0.5 * 2016.0, 0.5 * 0.5 * 2240.0], "x right, y down");
        local(|l| l.pair_aim.set(None));
    }
}
