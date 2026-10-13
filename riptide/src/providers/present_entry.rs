//! Provider: the present entry (`monaka_stereo::feature::Presents`: DXGI's Present, and Riptide's
//! steps around Monaka's present loop). The loop installs Riptide's eye site with it
//! (`EyeSite for Riptide`, [`eye_site`]); each published pair goes out with the frustum and eye
//! distance it was drawn with (`monaka_stereo::eye_site::loop_report`); a frame no view setup drew
//! goes to the flat screen; AFW publishes its own pairs (`monaka_stereo::afw11::ready`).
//!
//! Riptide's own steps: before each present the world-hidden report and the schedule's measurement
//! ([`before_present`]); after it the controllers are read and the eye site is given the next
//! frame's eye and pose ([`after_present`]); the end gives back what the run changed and reports
//! ([`finish`]).

use crate::Riptide;
use crate::providers::look_fields;
use crate::providers::{eye_site, hud_draws};
use monaka_body::look::facing;
use monaka_core::protocol::HeadPose;
use monaka_producer::log;
use monaka_stereo::driver::Driver;
use monaka_stereo::eye_site::{EyeSite, checks, loop_report};
use monaka_stereo::feature::{Graphics, Loop, Presents};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;

/// Riptide's steps around Monaka's present loop (this module): its eye site installed with the
/// loop; a frame goes flat when no view setup has drawn the world for a few presents
/// (`monaka_stereo::eye_site::world_hidden`); the HUD's draws end each frame; the end gives back
/// what the run changed.
impl Presents for Riptide {
    const LOOP: Loop<Self> = Loop {
        flat: Some(monaka_stereo::eye_site::world_hidden::<Riptide>),
        frame_end: Some(hud_draws::frame_end),
        report: Some(loop_report::<Riptide>),
        before: Some(before_present),
        after: Some(after_present),
        finish: Some(finish),
        eye_site: Some(<Riptide as EyeSite>::install),
        ..Loop::new(|_| Graphics::D3D11, ("camera shifts", &eye_site::VIEWS.drawn))
    };
}

/// Before present `n`: whether the world was drawn lately goes into the run's situations; a frame
/// the loop publishes as an eye (not AFW's own pair, not flat) is measured against the schedule.
pub fn before_present(driver: &Driver, _: &IDXGISwapChain, n: u64) -> bool {
    monaka_producer::situation::report(monaka_producer::situation::Situation::WorldHidden, !monaka_stereo::eye_site::world_drawn_recently::<Riptide>(), 500);
    if !monaka_stereo::afw11::ready::enabled() && driver.flat().is_none() {
        checks::note_label(n, eye_site::config().schedule);
    }
    false
}

/// After present `n`, with the pair's head `pose` chosen and recorded by the driver: the
/// culling relaxed again, the stick kept from the game, the controllers sampled for the next frame
/// (with hand aim, `monaka_body::hands::sample_frame`), the live camera turned to the view, and what
/// the next view setup draws from.
pub fn after_present(_: &Driver, n: u64, pose: &HeadPose) {
    let config = eye_site::config();
    eye_site::apply_mesh_culling(config.mesh_cull_scale);
    if config.aim.head {
        facing::keep_stick();
    }
    if config.aim.hand {
        monaka_body::hands::sample_frame();
    }
    eye_site::turn_at_present(pose, n);
    eye_site::choose(n, *pose);
}

/// Gives back everything the run changed, once, from whichever thread retires the eye (the
/// driver's finish), and reports the run.
pub fn finish(_: &Driver) {
    checks::report();
    eye_site::give_back();
    facing::release::<Riptide>();
    look_fields::report();
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
    monaka_body::hands::forget_frame();
    monaka_body::controllers::close();
    crate::providers::depth::release();
}
