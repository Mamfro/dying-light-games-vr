//! Provider: what the per-eye FSR upscaler (`monaka_stereo::upscaler::fsr11`) reads of DL1's draws
//! beyond its frame inputs ([`crate::providers::depth`]): the G-buffer's object motion (its fourth
//! target, R16G16_FLOAT; the camera left out; its units are not known, so it is added only with
//! `fsr_object_motion`), and its start from DL1's options ([`install`]). The HUD's draws
//! (`crate::providers::hud_draws`) hand the upscaler each frame's world image and HUD here.

use crate::providers::eye_site;
use crate::{Dl1, swapchain_parts};
use monaka_producer::game::{Lack, Missing, OrMissing, Start};
use monaka_producer::log;
use monaka_stereo::upscaler::fsr11;
use windows::Win32::Graphics::Direct3D11::{ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16G16_FLOAT;
use windows::core::Interface;

/// The upscaler's start: the eye size and DL1's FSR settings (`fsr`, `fsr_scale`, `fsr_sharpness`),
/// AMD's DLLs from `fsr_dlls=<folder>` or beside this DLL's copy.
pub fn install(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = crate::config(start);
    let (fsr, display) = config.upscaled().zip(config.eye_size).or_missing("FSR needs the headset's eye size and does not combine with AFW")?;
    let dlls = start.options.text("fsr_dlls").map_or_else(|| start.session.dir().to_path_buf(), std::path::PathBuf::from);
    let (_, device, _, render) = swapchain_parts(&start.build.renderer)?;
    // DL1's depth buffer is reversed with an infinite far plane (`engine::depth_buffer_projection`).
    let settings = fsr11::Start { render, display, sharpness: fsr.sharpness, jitter_sign: fsr.jitter_sign, depth_flags: fsr11::REVERSED_INFINITE, object_motion: fsr.object_motion };
    log!("{}", fsr11::start::<Dl1>(&device, settings, &dlls).missing()?);
    Ok(())
}

/// Whether the upscaler runs.
pub fn enabled() -> bool {
    fsr11::enabled()
}

/// Whether this frame still has to be upscaled (checked before any work on the hot draw path).
pub fn needs_capture() -> bool {
    fsr11::needs_capture()
}

/// Upscales the frame shown at present `n` (its world image is `color` now), once per frame.
pub fn capture(context: &ID3D11DeviceContext, color: &ID3D11Texture2D, n: u64) {
    if let Some(schedule) = eye_site::schedule() {
        fsr11::capture(context, color, n, schedule);
    }
}

/// The black and white targets this frame's HUD draws go into, begun at its first HUD draw.
pub fn hud_targets(context: &ID3D11DeviceContext, width: u32, height: u32) -> Option<[ID3D11RenderTargetView; 2]> {
    fsr11::hud_targets(context, width, height)
}

/// The game bound `count` render targets: the G-buffer's fourth (object motion) to the upscaler.
pub fn bound(count: u32, views: *const *mut core::ffi::c_void, depth: bool) {
    if let Some(texture) = motion_target(count, views, depth) {
        fsr11::object_motion(texture);
    }
}

/// The G-buffer's object motion when `count` render targets with a depth buffer are being bound:
/// the fourth target, R16G16_FLOAT (the camera's motion is not in it).
pub fn motion_target(count: u32, views: *const *mut core::ffi::c_void, depth: bool) -> Option<ID3D11Texture2D> {
    if count < 4 || views.is_null() || !depth {
        return None;
    }
    // SAFETY: the game passes an array of `count` views (each live or null).
    let raw = unsafe { *views.add(3) };
    // SAFETY: a live render target view or null.
    let view = unsafe { ID3D11RenderTargetView::from_raw_borrowed(&raw) }?;
    // SAFETY: COM call on a live view.
    let texture = unsafe { view.GetResource() }.ok()?.cast::<ID3D11Texture2D>().ok()?;
    (monaka_channel::d3d::texture_desc(&texture).Format == DXGI_FORMAT_R16G16_FLOAT).then_some(texture)
}
