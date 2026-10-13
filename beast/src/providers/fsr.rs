//! Provider: a frame-input source (`FrameInputs`, FSR's), and the frame's camera
//! (`FrameInputs::upscaled_place`, [`frame_place`]).
//!
//! FSR's inputs: the D3D12 renderer's imports of AMD's FidelityFX loader
//! (`monaka_framegen::ffx_imports`), for when the game upscales with FSR. The game then makes no
//! Streamline calls for its frames (DLSS's command alone sends its camera and tags), so at each
//! upscale dispatch the frame is labelled here by its camera, read from the renderer's frame data
//! as DLSS's constants carry it ([`frame_place`]); the frame inputs (`crate::providers::stereo`)
//! and the per-eye upscaler's FSR method (`crate::PER_EYE`: a twin of the game's
//! upscale context for the right eye) take the frame's eye from that label. With DLSS chosen in the
//! game, the game makes no FSR context and nothing here runs.

use crate::providers::engine;
use crate::Beast;
use monaka_framegen::ffx_imports::{self, Handlers, Install, PerEye};
use monaka_framegen::inputs::Place;
use monaka_framegen::upscale::DISPATCH_UPSCALE_KIND;
use monaka_hook::mem;
use monaka_producer::game::{Lack, OrMissing, Start};
use monaka_producer::log;
use monaka_stereo::feature::DRIVER;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering::Relaxed};

/// The renderer's base, its frame data read at each dispatch.
static RENDERER: AtomicUsize = AtomicUsize::new(0);
/// The eye the frame being upscaled was labelled with at its dispatch (-1: none).
static UPSCALED_EYE: AtomicI32 = AtomicI32::new(-1);

/// The FidelityFX imports' install (`crate::FSR`): each FSR upscale labels its frame with its eye
/// ([`dispatched`]); FSR per eye set up when [`crate::PER_EYE`] is on.
pub(crate) fn install_fsr(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let r12 = start.build.renderer_12.as_ref().or_missing("the game is not on D3D12")?;
    RENDERER.store(start.build.renderer.base(), Relaxed);
    let per_eye = start.is_on(&crate::PER_EYE);
    // Each eye's scene renders against its own previous frame (`crate::providers::eye_history`).
    monaka_framegen::upscale_per_eye::use_game_history(per_eye && crate::providers::eye_history::wanted(start.options));
    let install = Install { handlers: Handlers { dispatch: Some(dispatched), ..Handlers::default() }, per_eye: per_eye.then_some(PerEye { eye: upscaled_eye }) };
    // SAFETY: rd3d12 imports ffxCreateContext, ffxDispatch and ffxDestroyContext from AMD's loader.
    if unsafe { ffx_imports::install(start.hooks, r12, install) }? {
        log!("fsr: the renderer's FidelityFX imports hooked{}", if per_eye { "; FSR per eye once the game makes its upscale context" } else { "" });
    }
    Ok(())
}

/// `ffxDispatch`, an upscale: the frame labelled by its camera (`Driver::label_frame`), as DLSS's
/// constants label it.
fn dispatched(_: usize, desc: *const c_void) {
    if mem::read::<u64>(desc as usize) != Some(DISPATCH_UPSCALE_KIND) {
        return;
    }
    let eye = frame_place().and_then(|place| DRIVER.label_frame(place.position));
    UPSCALED_EYE.store(eye.map_or(-1, |eye| eye as i32), Relaxed);
}

/// The eye the frame being upscaled was labelled with at this dispatch.
fn upscaled_eye() -> Option<usize> {
    usize::try_from(UPSCALED_EYE.load(Relaxed)).ok()
}

/// Where the camera of the frame being rendered is, on the render thread while the frame records:
/// the renderer's frame data's main view, its camera-to-world's translation and axes, as DLSS's
/// constants carry them (the axes normalised, as there).
pub(crate) fn frame_place() -> Option<Place> {
    let base = RENDERER.load(Relaxed);
    if base == 0 {
        return None;
    }
    let renderer = mem::read::<usize>(base + engine::RENDERER_GLOBAL)?;
    let frame = mem::read::<usize>(renderer + engine::RENDERER_FRAME_DATA)?;
    let camera = mem::read::<usize>(frame + engine::FRAME_MAIN_VIEW)?;
    place_of(&mem::read_finite::<12>(camera + engine::CAMERA_INVERSE)?)
}

/// The place a camera-to-world 3x4 (rows of four, the translation last) gives: the columns are
/// right, up and forward (normalised), the last the position.
fn place_of(m: &[f32; 12]) -> Option<Place> {
    let column = |c: usize| [m[c], m[4 + c], m[8 + c]];
    let unit = |v: [f32; 3]| {
        let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        (length > 1e-6).then(|| v.map(|x| x / length))
    };
    Some(Place { position: column(3), right: unit(column(0))?, up: unit(column(1))?, forward: unit(column(2))? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_camera_matrix_gives_the_place_dlss_constants_carry() {
        // Turned 90 degrees about y, scaled by 2 (as a matrix may be), at (3, 1.7, -8).
        let m = [0.0, 0.0, 2.0, 3.0, 0.0, 2.0, 0.0, 1.7, -2.0, 0.0, 0.0, -8.0];
        let place = place_of(&m).unwrap();
        assert_eq!(place.position, [3.0, 1.7, -8.0]);
        assert_eq!((place.right, place.up, place.forward), ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]));
        assert!(place_of(&[0.0; 12]).is_none(), "no axes");
    }
}
