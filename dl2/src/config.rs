//! What the launcher can set, from `dl2-options.txt` beside the DLL copy: the shared settings
//! ([`Shared`], the manifest's defaults) and this game's own.

use crate::manifest::MANIFESTS;
use monaka_arms::aim::AimSettings;
use monaka_arms::{melee, roomscale};
use monaka_core::options::{Options, Shared};
use std::ops::Deref;
use std::sync::OnceLock;

/// `debug_flags=<bits>` for bisecting on PC.
pub mod debug {
    /// The native camera history (no per-eye history).
    pub const NATIVE_CAMERA_HISTORY: u32 = 1;
    /// The native DLSS history (no per-eye viewport or matrices).
    pub const NATIVE_DLSS_HISTORY: u32 = 2;
    /// Mono: no stereo at all.
    pub const MONO: u32 = 4;
    /// Show the left eye's present on the monitor too (D3D11).
    pub const SHOW_LEFT: u32 = 8;
    /// Sweep the synthetic head 15 degrees either way every 4 seconds.
    pub const SWEEP: u32 = 16;
    /// A fast synthetic head turn: 40 degrees either way each second.
    pub const FAST_SWEEP: u32 = 32;
    /// The right eye reuses the left eye's sun shadow maps (they do not fit it: lighter shadows).
    pub const SHARE_SHADOWS: u32 = 128;
    /// The right eye runs every once-per-frame command again (ray-tracing structures included).
    pub const ONCE_AGAIN: u32 = 256;
    /// Keep the headset's off-centre field of view (same-frame and alternate-eye eyes, and the
    /// depth-stereo centre view) instead of a centred one.
    pub const OFF_CENTRE: u32 = 512;
    /// A synthetic field of view reaching further down than up, as a headset's does.
    pub const LOW_FOV: u32 = 1024;
    /// The synthetic head's eyes get a symmetric field of view (a centred projection).
    pub const SYMMETRIC_FOV: u32 = 131072;
    /// Frame generation copies the frame-generation call's depth and motion vectors when tagged
    /// (before the game finishes them) instead of at the eye's present.
    pub const LATE_INPUTS_AT_TAG: u32 = 65536;
    /// Frame generation leaves the HUD off its generated frames.
    pub const NO_HUD_COMPOSE: u32 = 32768;
    /// Frame generation publishes each eye's world without the HUD in place of the generated frame.
    pub const SHOW_HUD_FREE: u32 = 8192;
    /// Frame generation interpolates the finished frames (the HUD moves with the world).
    pub const NO_HUDLESS: u32 = 2048;
    /// Mono frames publish the HUD-less colour as the left eye.
    pub const HUDLESS_LEFT: u32 = 4096;
    /// Log head aim and the baked yaw (a trace: read by `research::Options`).
    pub const AIM_LOG: u32 = 16384;
}

/// The DXRT commands (`engine::ONCE_COMMANDS` 10 to 13).
pub const ONCE_SKIP_DEFAULT: u32 = 0b1111 << 10;

/// The launcher's `mode` choice (`framegen`, `same`, `depth`, `alternate`; `auto` is settled by
/// the launcher before it gets here).
const MODES: &[(&str, Mode)] = &[("framegen", Mode::FrameGen), ("same", Mode::Same), ("depth", Mode::Depth), ("alternate", Mode::Alternate), ("auto", Mode::FrameGen)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Same,
    Alternate,
    Depth,
    FrameGen,
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub shared: Shared,
    /// What aims (`monaka_arms::aim`): the shared aim plus `melee_aim_head` and `tool_aim`.
    pub arms_aim: AimSettings,
    /// Swinging a controller like the melee weapon it holds (or punching, bare-handed) attacks
    /// (with the hand rig; `swing_attack=0` turns it off, `swing_speed=` tunes it).
    pub melee: melee::Settings,
    /// Physical melee (with the hand rig; `eng_chr::melee`). DL2 starts a light attack on the
    /// press (DL1 on the release), so swings keep the shared press.
    pub physical_melee: Option<melee::Physical>,
    /// Room-scale following (with the hand rig; `monaka_arms::roomscale`).
    pub room_scale: Option<roomscale::Settings>,
    /// The headset's recommended eye size (`eye_size=WxH`, from the OpenXR runtime).
    pub eye_size: Option<(u32, u32)>,
    /// One render per frame, eyes taking turns (`mode=alternate`, D3D12).
    pub alternate_eye: bool,
    /// One centre render per frame, both eyes from its depth (`mode=depth`, D3D12).
    pub depth_stereo: bool,
    /// FSR frame generation per eye (`mode=framegen`; same-frame stereo, D3D12).
    pub framegen: bool,
    /// Block vertical look while VR runs, letting through only input toward level (`lock_pitch`).
    pub lock_pitch: bool,
    pub debug: u32,
    /// Which of `engine::ONCE_COMMANDS` the right eye skips (bit per index; `once_skip=`).
    /// Default: the ray-tracing structures only. Measured 2026-10-05 in gameplay: skipping the wind
    /// commands gives the right eye trees in another wind state (they look like other models), and
    /// skipping spot shadows, probes and particles lights it differently; running those again costs
    /// about 1 ms, the ray-tracing structures about 6 ms.
    pub once_skip: u32,
}

impl Deref for Config {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.shared
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::from_options(&Options::default())
    }
}

impl Config {
    pub fn from_options(options: &Options) -> Self {
        let shared = Shared::from_options(options, &MANIFESTS[0]);
        let mode = options.choice("mode", MODES, Mode::FrameGen);
        Self {
            shared,
            arms_aim: AimSettings::from_options(options, shared.aim, true),
            melee: melee::Settings::from_options(options, shared.aim.side),
            physical_melee: melee::Physical::from_options(options, shared.aim.rig, false),
            room_scale: roomscale::Settings::from_options(options, &shared),
            eye_size: options.size("eye_size"),
            alternate_eye: mode == Mode::Alternate,
            depth_stereo: mode == Mode::Depth,
            framegen: mode == Mode::FrameGen,
            lock_pitch: options.switch("lock_pitch", false),
            debug: options.value::<u32>("debug_flags").unwrap_or(0),
            once_skip: options.value::<u32>("once_skip").unwrap_or(ONCE_SKIP_DEFAULT),
        }
    }

    pub fn has(&self, flag: u32) -> bool {
        self.debug & flag != 0
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

pub fn set(config: Config) {
    let _ = CONFIG.set(config);
}

/// The run's options; defaults until the start has read them.
pub fn get() -> Config {
    CONFIG.get().copied().unwrap_or_default()
}

/// Whether debug flag `flag` is set for this run.
pub fn debug(flag: u32) -> bool {
    CONFIG.get().is_some_and(|c| c.has(flag))
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
    fn modes_and_defaults() {
        let config = Config::default();
        assert!(config.framegen && !config.depth_stereo && !config.alternate_eye, "Smooth, as the manifest says");
        assert!(config.aim.head && config.aim.hand && config.aim.rig && config.hud.world && config.hud.markers);
        assert_eq!(config.hud.scale, 0.55);
        assert_eq!(config.once_skip, ONCE_SKIP_DEFAULT);
        let depth = Config::from_options(&Options::parse("mode=depth\nhud_scale=0.4\naim=off\nlock_pitch=1\ndebug_flags=5\nonce_skip=3"));
        assert!(depth.depth_stereo && !depth.framegen && !depth.aim.head && depth.lock_pitch);
        assert_eq!((depth.hud.scale, depth.debug, depth.once_skip), (0.4, 5, 3));
        assert!(depth.has(debug::MONO) && !depth.has(debug::SHOW_LEFT));
        let same = Config::from_options(&Options::parse("mode=same\nsynthetic_yaw=30\nsynthetic_pitch=-10"));
        assert!(!same.framegen && !same.depth_stereo && !same.alternate_eye);
        assert_eq!(same.synthetic, Some((30.0, -10.0)));
        assert!(Config::from_options(&Options::parse("mode=alternate")).alternate_eye);
    }

    #[test]
    fn probes_are_research() {
        use crate::research::Options as Probes;
        let none = Probes::from_options(&Options::default());
        assert!(!none.hands && !none.markers && !none.gui && !none.cameras && !none.hud && !none.aim_log);
        let all = Probes::from_options(&Options::parse("probe_hands=1\nprobe_markers=1\nprobe_gui=1\nprobe_cameras=1\nprobe_hud=1\ndebug_flags=16389"));
        assert!(all.hands && all.markers && all.gui && all.cameras && all.hud && all.aim_log);
        assert!(!Probes::from_options(&Options::parse("debug_flags=5")).aim_log);
    }
}
