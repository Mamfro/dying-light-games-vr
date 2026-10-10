//! What the launcher can set, from `beast-options.txt` beside the DLL copy: the shared settings
//! ([`Shared`], the manifest's defaults) and this game's own. The research probes' options are
//! read by `research::Options`.

use crate::manifest::MANIFESTS;
use monaka_arms::aim::AimSettings;
use monaka_arms::{melee, roomscale};
use monaka_core::options::{Options, Shared};
use std::ops::Deref;

#[derive(Clone, Debug)]
pub struct Config {
    pub shared: Shared,
    /// What aims (`monaka_arms::aim`): the shared aim plus `melee_aim_head` and `tool_aim`, as
    /// asked; what is possible depends on the game DLL (`AimSettings::only_if`).
    pub arms_aim: AimSettings,
    /// Swinging a controller like the melee weapon it holds attacks (with the hand rig;
    /// `swing_attack`, `swing_speed`).
    pub melee: melee::Settings,
    /// Physical melee (with the hand rig; `eng_chr::melee`). The Beast starts a light attack when
    /// it is let go, as DL1 does: swings press briefly.
    pub physical_melee: Option<melee::Physical>,
    /// The flashlight in VR (with stereo; [`crate::player::flashlight`]): its camera-movement sway
    /// held at zero (`flashlight_steady`, default on) and its screen-space shadows off
    /// (`flashlight_shadows=1` keeps them); `flashlight_source=x,y` sets its source offset (the
    /// game's: 0.2, -0.05), `flashlight_shadow_offset=` its shadow push; `probe_flashlight=1` logs
    /// its variables.
    pub flashlight: crate::player::flashlight::Settings,
    /// Room-scale following (with the hand rig; `monaka_arms::roomscale`).
    pub room_scale: Option<roomscale::Settings>,
    /// What the hands do with the game's own throw, bow and reload (`motion_throw`, `motion_bow`,
    /// `manual_reload`, with the hand rig; `eng_chr::handwork`).
    pub handwork: eng_chr::handwork::Options,
    /// The biters' front grabs held off (`disable_zombie_grabs`; `eng_chr::grabs`).
    pub disable_zombie_grabs: bool,
    /// Alternate-eye stereo on D3D12, published on the channel named in `live-channel.txt`
    /// (`stereo=0`: the probes only).
    pub stereo: bool,
    /// The headset's eye size (`eye_size=WxH`): the game renders at it, windowed, while VR runs.
    pub eye_size: Option<(u32, u32)>,
    /// Per-eye frame generation (`mode=framegen`): its own pairs, from the game's FFX loader.
    pub framegen: bool,
    /// `eye_test=<degrees>` turns the left eye left and the right eye right to check the order.
    pub eye_test_degrees: f32,
    /// `latency_test=<degrees>` turns every other pair's head to check the pose records' timing.
    pub latency_test_degrees: f32,
    /// `turn_yaw=<degrees>`: the player camera turned, the PC check that writing it reaches the
    /// picture (`camera_update`).
    pub turn_yaw: Option<f32>,
    /// `dlss_reset=1`: DLSS drops its history every frame, so it never blends the eyes.
    pub dlss_reset: bool,
    /// `dlss_per_eye=1`: each eye its own DLSS viewport (an experiment).
    pub dlss_per_eye: bool,
    /// With per-eye DLSS, the jitter per eye too (`dlss_jitter_per_eye`, default on).
    pub dlss_jitter_per_eye: bool,
    /// The motion-vector fix on DLSS's inputs (`dlss_fix_motion`, default on).
    pub dlss_fix_motion: bool,
}

impl Deref for Config {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.shared
    }
}

impl Config {
    pub fn from_options(options: &Options) -> Self {
        let shared = Shared::from_options(options, &MANIFESTS[0]);
        let stereo = options.switch("stereo", true);
        Self {
            shared,
            arms_aim: AimSettings::from_options(options, shared.aim, true),
            melee: melee::Settings::from_options(options, shared.aim.side),
            physical_melee: melee::Physical::from_options(options, shared.aim.rig, true),
            flashlight: crate::player::flashlight::Settings {
                steady: stereo && options.switch("flashlight_steady", true),
                shadow_offset: options.value::<f32>("flashlight_shadow_offset").filter(|v| v.is_finite() && v.abs() <= 2.0),
                shadow_scale: (stereo && !options.switch("flashlight_shadows", false)).then_some(0.0),
                source: options.floats::<2>("flashlight_source", 3.0),
                probe: options.switch("probe_flashlight", false),
            },
            room_scale: roomscale::Settings::from_options(options, &shared),
            disable_zombie_grabs: options.switch("disable_zombie_grabs", true),
            handwork: eng_chr::handwork::Options {
                throw: options.switch("motion_throw", true) && shared.aim.rig,
                bow: options.switch("motion_bow", true) && shared.aim.rig,
                reload: options.switch("manual_reload", true) && shared.aim.rig,
                lockpick: false,
            },
            stereo,
            eye_size: options.size("eye_size").filter(|_| stereo),
            framegen: options.choice("mode", &[("framegen", true), ("standard", false)], options.switch("framegen", false)),
            eye_test_degrees: options.angle("eye_test", 30.0).unwrap_or(0.0),
            latency_test_degrees: options.angle("latency_test", 30.0).unwrap_or(0.0),
            turn_yaw: options.angle("turn_yaw", 90.0).filter(|&yaw| yaw != 0.0),
            dlss_reset: options.switch("dlss_reset", false),
            dlss_per_eye: options.switch("dlss_per_eye", false),
            dlss_jitter_per_eye: options.switch("dlss_jitter_per_eye", true),
            dlss_fix_motion: options.switch("dlss_fix_motion", true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_manifest_option() {
        let unread = monaka_core::manifest::unread_options(&MANIFESTS[0], |options| {
            Config::from_options(options);
        });
        assert!(unread.is_empty(), "not read: {unread:?}");
    }

    #[test]
    fn defaults_and_modes() {
        let config = Config::from_options(&Options::parse(""));
        assert!(config.stereo && config.aim.head && !config.aim.rig && !config.framegen && config.hud.world, "look-based, as the manifest says");
        assert_eq!(config.schedule.latency, 2);
        assert_eq!(config.eye_size, None);
        let config = Config::from_options(&Options::parse("mode=framegen\naim=hand_rig\neye_size=2160x2160\nlatency=3\nsynthetic_yaw=20\nsynthetic_pitch=5\neye_test=10"));
        assert!(config.framegen && config.aim.rig && config.arms_aim.hand_rig);
        assert_eq!((config.eye_size, config.schedule.latency, config.synthetic, config.eye_test_degrees), (Some((2160, 2160)), 3, Some((20.0, 5.0)), 10.0));
        assert!(Config::from_options(&Options::parse("framegen=1")).framegen, "the older switch");
        assert_eq!(Config::from_options(&Options::parse("stereo=0\neye_size=2160x2160")).eye_size, None, "no eye size without stereo");
    }
}
