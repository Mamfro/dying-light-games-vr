//! What the launcher can set, from `riptide-options.txt` beside the DLL: the shared settings
//! ([`Shared`], the manifest's defaults) and this game's own. Defaults are the headset play mode.

use monaka_body::look::aim::AimSettings;
use monaka_core::camera::Frustum;
use monaka_core::manifest::{Manifest, Opt};
use monaka_core::options::{Options, Shared};
use monaka_core::protocol::HeadPose;
use monaka_core::rendering::{self, STEREO_OFF, StereoMethod};
use std::ops::Deref;

/// The stereo methods Riptide offers: AFW (Frame-warp: each frame warped into the other eye with
/// its depth, on D3D11) by default, AER (Alt-eye).
pub const STEREO: Opt = rendering::stereo_method(&[StereoMethod::Aer.offer(), StereoMethod::Afw.offer(), STEREO_OFF], StereoMethod::Afw);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub shared: Shared,
    /// What aims (`monaka_body::look::aim`).
    pub aim_settings: AimSettings,
    /// Also turn the live player camera, so game-side visibility and aim follow the head.
    pub turn_live_camera: bool,
    /// With the live camera turned: turned also on the game's thread, right after the game builds
    /// it each update (`turn_on_update=0`: only at presents), so the HUD the game lays out next
    /// projects its world-anchored prompts through the view's camera every frame.
    pub turn_on_update: bool,
    /// Ask the game for a field of view wide enough for both eyes, so the edges are not culled.
    pub widen_game_fov: bool,
    /// Turn raster occlusion off: it is built from the camera this producer moves and hides
    /// distant buildings at the centre of view.
    pub no_raster_occlusion: bool,
    /// Scale of the level's mesh size-cull limits; the headset view draws everything smaller.
    pub mesh_cull_scale: f32,
    /// Detail zoom written to the render camera (1..16), off when absent.
    pub detail_zoom: Option<f32>,
    /// Experiment: render each eye with the smallest centred frustum containing it and report
    /// that, so screen-space effects that assume a centred projection see one. Costs about 20%
    /// more pixels.
    pub symmetric_projection: bool,
    /// AFW (`stereo=afw`, `monaka_stereo::afw11`): a pair every frame, the other eye reprojected
    /// from the real one, its gaps filled from that eye's last real frame.
    pub afw: bool,
}

impl Deref for Config {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.shared
    }
}

impl Config {
    pub fn from_options(options: &Options, manifest: &Manifest) -> Self {
        let shared = Shared::from_options(options, manifest);
        Self {
            shared,
            aim_settings: AimSettings::from_options(options, shared.aim, true),
            // Head aim turns the character itself, so its camera already looks where the head does.
            // Hand aim points that camera along the controller: turning it back to the view keeps
            // culling and shadows on what the head sees.
            turn_live_camera: options.switch("turn_live_camera", true) && (!shared.aim.head || shared.aim.hand),
            turn_on_update: options.switch("turn_on_update", true),
            widen_game_fov: options.switch("widen_game_fov", true),
            no_raster_occlusion: (shared.head_tracking || shared.synthetic.is_some()) && !options.switch("raster_occlusion", false),
            mesh_cull_scale: options.number("mesh_cull_scale", 0.001, 1.0, 0.2),
            detail_zoom: options.value::<f32>("detail_zoom").filter(|z| (1.0..=16.0).contains(z)),
            symmetric_projection: options.switch("symmetric_projection", false),
            afw: StereoMethod::chosen(&STEREO, options) == StereoMethod::Afw,
        }
    }

    /// The HUD and menus are placed in front of the eyes (`hud=0`: left as the game draws them).
    pub fn hud_active(&self) -> bool {
        self.hud.shown
    }

    /// The frustum eye `eye` is rendered with for `pose`: the headset's own, or its centred
    /// envelope with `symmetric_projection`.
    pub fn rendered_frustum(&self, pose: &HeadPose, eye: usize) -> Option<Frustum> {
        self.shared.rendered_frustum(pose, eye, self.symmetric_projection, None)
    }
}

/// A vertical field of view (degrees, 16:9) containing both eyes plus a margin. Widening only the
/// frustum edges leaves the bottom culled when looking up; a wider field of view does not.
pub fn widened_fov(pose: &HeadPose) -> Option<f32> {
    let (mut horizontal, mut vertical) = (0.0f32, 0.0f32);
    for eye in &pose.fov {
        horizontal = horizontal.max(eye[0].tan().abs()).max(eye[1].tan().abs());
        vertical = vertical.max(eye[2].tan().abs()).max(eye[3].tan().abs());
    }
    let degrees = 2.0 * vertical.max(horizontal * 9.0 / 16.0).atan().to_degrees() + 8.0;
    (degrees.is_finite() && degrees < 170.0).then_some(degrees)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::MANIFESTS;
    use monaka_core::protocol::LEFT_HAND;

    impl Config {
        fn parse(text: &str) -> Self {
            Self::from_options(&Options::parse(text), &MANIFESTS[0])
        }
    }

    #[test]
    fn defaults_are_the_headset_mode() {
        let config = Config::parse("");
        assert_eq!(config.schedule.latency, 2);
        assert!(config.head_tracking && config.headset_fov && config.aim.head && config.widen_game_fov);
        assert!(!config.aim.rig && !config.aim.hand, "head aim: no hand rig in this game");
        assert!(!config.turn_live_camera, "head aim replaces the live-camera turn");
        assert!(config.no_raster_occlusion && config.hud_active());
        assert_eq!((config.hud.scale, config.hud.distance, config.mesh_cull_scale), (0.36, 2.0, 0.2));
        assert_eq!((config.synthetic, config.detail_zoom), (None, None));
        assert!(!config.symmetric_projection, "experiments are opt-in");
        assert!(config.afw, "AFW the method");
        let hands = Config::parse("aim=controller\naim_hand=Left");
        assert!(hands.aim.hand && hands.aim.side == LEFT_HAND && !hands.aim.rig && hands.aim_settings.hand_aim);
        assert!(hands.turn_live_camera, "hand aim turns the live camera back to the view");
        let without = Config::parse("aim=off");
        assert!(!without.aim.head && without.turn_live_camera);
    }

    #[test]
    fn options_override_and_are_bounded() {
        let options = Options::parse("latency=9\nhud=0\nsynthetic_yaw=30\nhead_tracking=off\ndetail_zoom=0.5");
        let config = Config::from_options(&options, &MANIFESTS[0]);
        assert_eq!(config.schedule.latency, 2, "out-of-range latency keeps the default");
        assert!(!config.hud_active());
        assert_eq!(config.synthetic, Some((30.0, 0.0)));
        assert!(config.no_raster_occlusion, "a synthetic pose still turns occlusion off");
        assert_eq!(config.detail_zoom, None);
    }

    #[test]
    fn widened_fov_contains_both_eyes() {
        let config = Config::parse("");
        let pose = config.synthetic_pose(0.0, 0.0);
        let fov = widened_fov(&pose).unwrap();
        // The vertical half-angle 0.80 dominates: 2*0.80 radians in degrees, plus 8.
        assert!((fov - (2.0 * 0.80f32.to_degrees() + 8.0)).abs() < 0.01, "{fov}");
    }
}
