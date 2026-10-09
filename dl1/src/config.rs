//! What the launcher can set, from `dl1-options.txt` beside the DLL: the shared settings
//! ([`Shared`], the manifest's defaults) and this game's own. Defaults are the headset play mode.

use crate::manifest::MANIFESTS;
use monaka_arms::aim::AimSettings;
use monaka_arms::{melee, roomscale};
use monaka_core::camera::Frustum;
use monaka_core::options::{Options, Shared};
use monaka_core::protocol::HeadPose;
use std::ops::Deref;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub shared: Shared,
    /// What aims (`monaka_arms::aim`): the shared aim plus `melee_aim_head` and `tool_aim`.
    pub arms_aim: AimSettings,
    /// Swinging a controller like the melee weapon it holds (or punching with it, bare-handed)
    /// attacks (needs the hand rig; `swing_attack=0` turns it off, `swing_speed=` tunes it).
    pub melee: melee::Settings,
    /// Also turn the live player camera, so game-side visibility and aim follow the head.
    pub turn_live_camera: bool,
    /// With the live camera turned: turned also on the game's thread, right after the game builds
    /// it each update (`turn_on_update=0`: only at presents), so the HUD the game lays out next
    /// projects its world-anchored prompts through the view's camera every frame.
    pub turn_on_update: bool,
    /// Ask the game for a field of view wide enough for both eyes, so the edges are not culled.
    pub widen_game_fov: bool,
    /// Turn raster occlusion off: it is built from the camera this producer moves and hid
    /// distant buildings at the centre of view.
    pub no_raster_occlusion: bool,
    /// Scale of the level's mesh size-cull limits; the headset view draws everything smaller.
    pub mesh_cull_scale: f32,
    /// Detail zoom written to the render camera (1..16), off when absent.
    pub detail_zoom: Option<f32>,
    /// Experiment: render each eye with the smallest centred frustum containing it and report
    /// that, so screen-space effects that assume a centred projection see one (DL2: fixed shadow
    /// blocks). Costs about 20% more pixels.
    pub symmetric_projection: bool,
    /// Experiment: alternate-eye rendering with depth (`monaka_stereo::hybrid`): a pair every frame,
    /// the other eye reprojected from the real one, its gaps filled from that eye's last real frame.
    pub hybrid: bool,
    /// With the in-world HUD, its pieces at full opacity whatever the game fades them to (health at
    /// full health, the quest list after a while): `world_hud_opaque=0` lets the game fade them.
    pub world_hud_opaque: bool,
    /// Moves the wrist this far in the palm's space (`hand_hold=x,y,z`, metres; x right, y up along
    /// the knuckles, z back along the pointing finger) when the hand sits wrong on the controller.
    pub hand_hold: [f32; 3],
    /// Climbing or hanging (the hand rig's climbs), the view backs this far off the wall, along
    /// the way the player faced on grabbing it (`climb_pullback=`, metres; 0 keeps it in place):
    /// at the wall, nothing of the climb could be made out.
    pub climb_pullback: f32,
    /// Diagnostic: the real pad's input is dropped from the reads the VR pad merges into.
    pub pad_mask_real: bool,
    /// Room-scale following (with the hand rig; `monaka_arms::roomscale` on
    /// [`crate::player::roomscale`]).
    pub room_scale: Option<roomscale::Settings>,
    /// Physical melee (with the hand rig; [`crate::player::melee`]). DL1 starts a light attack
    /// when it is let go: swings press briefly.
    pub physical_melee: Option<melee::Physical>,
    /// The headset's eye image size (`eye_size=WxH`, from `monaka_viewer --info`): the game renders
    /// at it while VR runs and gets its own size back at the stop.
    pub eye_size: Option<(u32, u32)>,
    /// FSR upscaling (`fsr=1`, needs `eye_size`): the game renders each eye smaller and AMD's FSR
    /// brings it to the eye size.
    pub fsr: Option<Fsr>,
}

impl Deref for Config {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.shared
    }
}

/// FSR upscaling settings (`fsr_*` options).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fsr {
    /// Eye size over render size per side (`fsr_scale`; 1.5 is AMD's Quality, 2 Performance).
    pub scale: f32,
    /// RCAS sharpening 0..1 (`fsr_sharpness`; 0 turns it off).
    pub sharpness: Option<f32>,
    /// The G-buffer's object motion to UV units (`fsr_object_motion`; 0 leaves it out, which it
    /// does until its units are measured).
    pub object_motion: f32,
    /// Whether the dispatch is told the jitter's content shift (1) or its opposite (-1). 1, as
    /// AMD documents it, looked much sharper in the headset than -1 (blind A/B, 2026-10-06).
    pub jitter_sign: f32,
}

impl Fsr {
    /// The render size for an eye of `eye` size: each side divided by the scale, rounded to 8.
    pub fn render_size(&self, eye: (u32, u32)) -> (u32, u32) {
        let side = |v: u32| (((v as f32 / self.scale) / 8.0).round() as u32).max(8) * 8;
        (side(eye.0), side(eye.1))
    }
}

impl Config {
    pub fn from_options(options: &Options) -> Self {
        let shared = Shared::from_options(options, &MANIFESTS[0]);
        let arms_aim = AimSettings::from_options(options, shared.aim, true);
        Self {
            shared,
            arms_aim,
            melee: melee::Settings::from_options(options, shared.aim.side),
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
            hybrid: options.switch("hybrid", false),
            world_hud_opaque: options.switch("world_hud_opaque", true),
            climb_pullback: options.number("climb_pullback", 0.0, 1.5, 0.35),
            hand_hold: options.floats::<3>("hand_hold", 1.0).unwrap_or([0.0; 3]),
            pad_mask_real: options.switch("pad_mask_real", false),
            room_scale: roomscale::Settings::from_options(options, &shared),
            physical_melee: melee::Physical::from_options(options, shared.aim.rig, true),
            eye_size: options.size("eye_size"),
            fsr: options.switch("fsr", false).then(|| Fsr {
                scale: options.number("fsr_scale", 1.0, 3.0, 1.5),
                sharpness: Some(options.number("fsr_sharpness", 0.0, 1.0, 0.3)).filter(|&s| s > 0.0),
                object_motion: options.number("fsr_object_motion", -100.0, 100.0, 0.0),
                jitter_sign: if options.number("fsr_jitter_sign", -1.0, 1.0, 1.0) < 0.0 { -1.0 } else { 1.0 },
            }),
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

/// A vertical field of view (degrees, 16:9) containing both eyes plus a margin. Headset tests:
/// widening only the frustum edges left the bottom culled when looking up; a wider value did not.
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
    use monaka_core::protocol::{LEFT_HAND, RIGHT_HAND};

    #[test]
    fn reads_every_manifest_option() {
        let unread = monaka_core::manifest::unread_options(&MANIFESTS[0], |options| {
            Config::from_options(options);
        });
        assert!(unread.is_empty(), "not read: {unread:?}");
    }

    #[test]
    fn fsr_keeps_the_dynamic_hud() {
        // The hands' panels take their pieces' draws in the upscaler's HUD layer path too.
        assert!(Config::from_options(&Options::parse("fsr=1\nworld_hud=1")).hud.world);
    }

    #[test]
    fn defaults_are_the_headset_mode() {
        let config = Config::from_options(&Options::default());
        assert_eq!(config.schedule.latency, 2);
        assert!(config.head_tracking && config.headset_fov && config.aim.head && config.widen_game_fov);
        assert!(config.aim.rig && config.aim.hand, "motion controls, as the manifest says");
        assert!(config.turn_live_camera, "hand aim turns the live camera back to the view");
        assert!(!Config::from_options(&Options::parse("aim=head")).turn_live_camera, "head aim replaces the live-camera turn");
        assert!(config.no_raster_occlusion && config.hud_active() && config.hud.world);
        assert_eq!((config.hud.scale, config.hud.distance, config.mesh_cull_scale), (0.36, 2.0, 0.2));
        assert_eq!((config.synthetic, config.detail_zoom), (None, None));
        assert!(!config.symmetric_projection && !config.hybrid, "experiments are opt-in");
        assert!(config.fsr.is_none(), "FSR is opt-in");
        let without = Config::from_options(&Options::parse("aim=off"));
        assert!(!without.aim.head && without.turn_live_camera && !without.arms_aim.head_aim);
        let hands = Config::from_options(&Options::parse("aim=controller\naim_hand=Left"));
        assert!(hands.aim.hand && hands.aim.side == LEFT_HAND && !hands.aim.rig && hands.arms_aim.hand_aim);
        assert!(hands.turn_live_camera, "hand aim turns the live camera back to the view");
        assert_eq!(Config::from_options(&Options::parse("aim=head")).aim.side, RIGHT_HAND);
        let rig = Config::from_options(&Options::parse("aim=hand_rig\nhand_hold=-0.02, 0.01,0.03"));
        assert!(rig.aim.rig && rig.hand_hold == [-0.02, 0.01, 0.03]);
        assert!(rig.melee.swing && rig.arms_aim.melee_aim_head, "melee by swinging comes with the rig");
        assert_eq!(rig.physical_melee, Some(melee::Physical { blade: 1.1, fist: 0.15, press_ms: Some(40) }), "and physical melee");
        assert!(Config::from_options(&Options::parse("aim=head")).physical_melee.is_none());
        assert!(!Config::from_options(&Options::parse("swing_attack=0")).melee.swing);
        assert_eq!(Config::from_options(&Options::parse("hand_hold=0,2,0")).hand_hold, [0.0; 3]);
        assert_eq!(Config::from_options(&Options::parse("hand_hold=0,0")).hand_hold, [0.0; 3]);
    }

    #[test]
    fn rendered_frustum_follows_the_experiment() {
        let mut config = Config::from_options(&Options::default());
        let pose = config.synthetic_pose(0.0, 0.0);
        let own = config.rendered_frustum(&pose, 0).unwrap();
        assert!(own.straight_ahead()[0] > 0.0, "the headset's own off-centre frustum");
        config.symmetric_projection = true;
        assert_eq!(config.rendered_frustum(&pose, 0).unwrap().straight_ahead(), [0.0, 0.0]);
        assert!(config.rendered_frustum(&HeadPose::default(), 0).is_none());
    }

    #[test]
    fn options_override_and_are_bounded() {
        let options = Options::parse("latency=9\nhud=0\nsynthetic_yaw=30\nhead_tracking=off\ndetail_zoom=0.5");
        let config = Config::from_options(&options);
        assert_eq!(config.schedule.latency, 2, "out-of-range latency keeps the default");
        assert!(!config.hud_active());
        assert_eq!(config.synthetic, Some((30.0, 0.0)));
        assert!(config.no_raster_occlusion, "a synthetic pose still turns occlusion off");
        assert_eq!(config.detail_zoom, None);
        assert_eq!(config.eye_size, None);
        assert_eq!(Config::from_options(&Options::parse("eye_size=2644x2644")).eye_size, Some((2644, 2644)));
        assert_eq!(Config::from_options(&Options::parse("eye_size=100x2644")).eye_size, None);
    }

    #[test]
    fn widened_fov_contains_both_eyes() {
        let config = Config::from_options(&Options::default());
        let pose = config.synthetic_pose(0.0, 0.0);
        let fov = widened_fov(&pose).unwrap();
        // The vertical half-angle 0.80 dominates: 2*0.80 radians in degrees, plus 8.
        assert!((fov - (2.0 * 0.80f32.to_degrees() + 8.0)).abs() < 0.01, "{fov}");
    }

    #[test]
    fn fsr_renders_smaller_in_eights() {
        let config = Config::from_options(&Options::parse("fsr=1\neye_size=2644x2644"));
        let fsr = config.fsr.expect("on");
        assert_eq!(fsr.render_size((2644, 2644)), (1760, 1760));
        assert_eq!(fsr.sharpness, Some(0.3));
        let off = Config::from_options(&Options::parse("fsr=1\nfsr_sharpness=0\nfsr_scale=2"));
        assert_eq!(off.fsr.map(|f| (f.sharpness, f.render_size((2644, 2644)))), Some((None, (1320, 1320))));
    }
}
