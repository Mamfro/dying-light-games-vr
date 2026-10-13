//! How The Beast renders (`monaka_core::rendering`): the stereo method, AER (Alt-eye) or AFW
//! (Frame-warp), and frame generation, which the stereo components' conditions read. Frame generation
//! runs per eye with AER and as in-between pairs with AFW.

use monaka_core::manifest::Opt;
use monaka_core::options::Options;
use monaka_core::rendering::{self, FrameGenerator, STEREO_OFF, StereoMethod};

/// The stereo methods The Beast offers: AER (Alt-eye) by default, AFW (Frame-warp) marked experimental
/// (its warped eye ghosts a moving hand and borrows the other eye's flashlight light).
pub(crate) const STEREO: Opt = rendering::stereo_method(&[StereoMethod::Aer.offer(), StereoMethod::Afw.experimental(), STEREO_OFF], StereoMethod::Aer);
/// The frame generators The Beast offers: off by default, FSR's.
pub(crate) const FRAMEGEN: Opt = rendering::frame_generation(&[FrameGenerator::Off.offer(), FrameGenerator::Fsr.offer()], FrameGenerator::Off);

/// The run's stereo method (`stereo`).
pub(crate) fn method(options: &Options) -> StereoMethod {
    StereoMethod::chosen(&STEREO, options)
}

/// Frame generation is wanted (`framegen=fsr`): per eye with AER, in-between pairs with AFW.
pub(crate) fn framegen_on(options: &Options) -> bool {
    FrameGenerator::chosen(&FRAMEGEN, options) == FrameGenerator::Fsr
}

#[cfg(test)]
mod lift_checks;
#[cfg(test)]
mod tests;
