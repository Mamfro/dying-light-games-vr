//! Provider: the present entry (`monaka_stereo::feature::Presents`: DXGI's Present, and DL1's steps
//! around Monaka's present loop). The loop installs DL1's eye site with it
//! (`EyeSite for Dl1`, [`eye_site`]); each published pair goes out with the frustum and eye distance
//! it was drawn with (`monaka_stereo::eye_site::loop_report`); a frame no view setup drew goes to the
//! flat screen in its own shape (the UI's fixed size, `crate::extras::ui_size`); the upscaler and the
//! AFW publish their own pairs (`monaka_stereo::upscaler::fsr11`, `monaka_stereo::afw11::ready`).
//!
//! DL1's own steps: before each present the world-hidden report and the schedule's measurement
//! ([`before_present`]); after it the controllers are read and the eye site is given the next
//! frame's eye and pose ([`after_present`]); the end gives back what the run changed and reports
//! ([`finish`]).

use crate::Dl1;
use crate::config::Config;
use crate::providers::look_fields;
use crate::providers::{eye_site, hud_draws};
use monaka_arms::owned_facing::FACING;
use monaka_core::protocol::HeadPose;
use monaka_producer::log;
use monaka_stereo::driver::Driver;
use monaka_stereo::eye_site::{EyeSite, checks, loop_report};
use monaka_stereo::feature::{DRIVER, Graphics, Loop, Presents};
use std::sync::atomic::{AtomicBool, Ordering::*};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;

/// DL1's steps around Monaka's present loop (this module): its eye site installed with the loop; a
/// frame goes flat when no view setup has drawn the world for a few presents
/// (`monaka_stereo::eye_site::world_hidden`), in its own shape; the HUD's draws end each frame; the
/// end gives back what the run changed.
impl Presents for Dl1 {
    const LOOP: Loop<Self> = Loop {
        flat: Some(monaka_stereo::eye_site::world_hidden::<Dl1>),
        flat_picture: Some(flat_picture),
        frame_end: Some(hud_draws::frame_end),
        report: Some(loop_report::<Dl1>),
        before: Some(before_present),
        after: Some(after_present),
        finish: Some(finish),
        eye_site: Some(<Dl1 as EyeSite>::install),
        ..Loop::new(|_| Graphics::D3D11, ("camera shifts", &eye_site::VIEWS.drawn))
    };
}

/// A flat frame of `size` holds the game's UI at its fixed size while VR runs, stretched over it.
fn flat_picture(size: (u32, u32)) -> (u32, u32) {
    crate::extras::ui_size::fixed().unwrap_or(size)
}

/// Before present `n`: whether the world was drawn lately goes into the run's situations; a frame
/// the loop publishes as an eye (no feature's own pair, not flat) is measured against the schedule.
pub fn before_present(driver: &Driver, _: &IDXGISwapChain, n: u64) -> bool {
    monaka_producer::situation::report(monaka_producer::situation::Situation::WorldHidden, !monaka_stereo::eye_site::world_drawn_recently::<Dl1>(), 500);
    let own_pairs = monaka_stereo::upscaler::fsr11::enabled() || monaka_stereo::afw11::ready::enabled();
    if !own_pairs && driver.flat().is_none() {
        checks::note_label(n, eye_site::config().schedule);
    }
    false
}

/// After present `n`, with the pair's head `pose` chosen and recorded by the driver: the
/// culling relaxed again, the stick kept from the game, the controllers sampled for the next frame
/// (with hand aim, `monaka_arms::hands::sample_frame`), the live camera turned to the view, and what
/// the next view setup draws from.
pub fn after_present(_: &Driver, n: u64, pose: &HeadPose) {
    let config = eye_site::config();
    eye_site::apply_mesh_culling(config.mesh_cull_scale);
    if config.aim.head {
        FACING.keep_stick();
    }
    if config.aim.hand {
        monaka_arms::hands::sample_frame();
    }
    let pose = *pose;
    suggest_render_size(config, &pose);
    eye_site::turn_at_present(&pose, n);
    eye_site::choose(n, pose);
}

/// Once per run: the game resolution whose aspect matches the eye frustum actually rendered.
/// The image is 16:9 for a roughly square eye view, which wastes pixels and sharpness; set it with
/// `tools\Set-DL1Resolution.ps1` (needs a game restart).
fn suggest_render_size(config: &Config, pose: &HeadPose) {
    static SUGGESTED: AtomicBool = AtomicBool::new(false);
    if SUGGESTED.load(Relaxed) {
        return;
    }
    let height = DRIVER.with_publisher11(|p| p.size().1).unwrap_or(0);
    if let Some(frustum) = config.rendered_frustum(pose, 0)
        && height > 0
        && !SUGGESTED.swap(true, Relaxed)
    {
        let (width, height) = frustum.render_size(height);
        log!("eye frustum aspect {:.3}: a {width}x{height} game resolution would match it", width as f32 / height as f32);
    }
}

/// Gives back everything the run changed, once, from whichever thread retires the eye (the
/// driver's finish), and reports the run.
pub fn finish(_: &Driver) {
    checks::report();
    eye_site::give_back();
    monaka_arms::owned_facing::release::<Dl1>();
    look_fields::report();
    crate::providers::skeleton::report();
    crate::research::finish();
    crate::providers::hud_pieces::report();
    let c = eye_site::counts();
    log!(
        "run ended: {}; head_applied={} head_reapplied={} live_turns={} widened_fov={} hud_draws_shifted={} draws_seen={} scene_copies={} hud_layer_draws={}",
        eye_site::VIEWS.summary(),
        c.head_applied,
        c.head_reapplied,
        c.live_turns,
        c.widened_fov,
        hud_draws::shifted_draws(),
        hud_draws::draws_seen(),
        hud_draws::scene_copies(),
        hud_draws::layer_draws(),
    );
}

/// Forgets the frame's controllers, releases their block and the scene depth after the hooks are off.
pub fn release() {
    monaka_arms::hands::forget_frame();
    monaka_arms::controllers::close();
    crate::providers::depth::release();
    crate::research::release();
}
