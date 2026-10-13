//! The stereo's settings for the run, read once at the start ([`set`]) for the hooks to ask
//! ([`get`], [`debug`]): the shared settings (the manifest's defaults), the stereo method and frame
//! generation, and the PC checks' debug flags. The components read their own options at their install.

use crate::manifest::MANIFESTS;
use monaka_core::manifest::{Manifest, Opt};
use monaka_core::options::{Options, Shared};
use monaka_core::rendering::{self, FrameGenerator, STEREO_OFF, StereoMethod};
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

/// The stereo methods DL2 offers (`monaka_core::rendering`): Standard (Same-frame) by default, AER
/// (Alt-eye), Depth-stereo (synthetic). Depth-stereo shows the hands no nearer than its nearest
/// comfortable depth: the panels on them too.
pub const STEREO: Opt = rendering::stereo_method(&[StereoMethod::SameFrame.offer(), StereoMethod::Aer.offer(), StereoMethod::DepthStereo.offer(), STEREO_OFF], StereoMethod::SameFrame).viewer_args("depth", &["--panel-nearest", "0.5"]);
/// The frame generators DL2 offers: FSR's by default (per eye on the same-frame pairs), off. DL2
/// generates frames only on the same-frame pairs, so the choice is offered only with Standard
/// (Same-frame).
pub const FRAMEGEN: Opt = rendering::frame_generation(&[FrameGenerator::Off.offer(), FrameGenerator::Fsr.offer()], FrameGenerator::Fsr).only_when("stereo", "same");

/// Whether the run asks for frame generation (`framegen=fsr`, the default), whichever method it
/// picked.
pub fn framegen_asked(options: &Options) -> bool {
    FrameGenerator::chosen(&FRAMEGEN, options) == FrameGenerator::Fsr
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub shared: Shared,
    /// One render per frame, eyes taking turns (`stereo=aer`, D3D12).
    pub alternate_eye: bool,
    /// One centre render per frame, both eyes from its depth (`stereo=depth`, D3D12).
    pub depth_stereo: bool,
    /// FSR frame generation per eye (`framegen=fsr` with `stereo=same`, D3D12; with the other
    /// methods DL2 has none).
    pub framegen: bool,
    pub debug: u32,
    /// Which of `engine::ONCE_COMMANDS` the right eye skips (bit per index; `once_skip=`).
    /// Default: the ray-tracing structures only. Skipping the wind commands gives the right eye
    /// trees in another wind state (they look like other models), and skipping spot shadows, probes
    /// and particles lights it differently; running those again costs about 1 ms, the ray-tracing
    /// structures about 6 ms.
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
        Self::from_options(&Options::default(), &MANIFESTS[0])
    }
}

impl Config {
    pub fn from_options(options: &Options, manifest: &Manifest) -> Self {
        let method = StereoMethod::chosen(&STEREO, options);
        Self {
            shared: Shared::from_options(options, manifest),
            alternate_eye: method == StereoMethod::Aer,
            depth_stereo: method == StereoMethod::DepthStereo,
            framegen: method == StereoMethod::SameFrame && framegen_asked(options),
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
mod lift_checks;
#[cfg(test)]
mod tests;
