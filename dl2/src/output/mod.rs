//! How finished images leave the game: the eye images taken from the back buffer at each
//! present request and published (`render11`, `render12`), the depth warp and the HUD lay-over
//! (`warp12`), FSR frame generation per eye (`framegen`), per-eye DLSS and its history (`dlss`,
//! `temporal`), the Streamline tags they read (`streamline`), and the switch to the headset's eye
//! size (`video`).

pub(crate) mod dlss;
pub(crate) mod framegen;
pub(crate) mod render11;
pub(crate) mod render12;
pub(crate) mod streamline;
pub(crate) mod temporal;
pub(crate) mod video;
pub(crate) mod warp12;

use crate::view::scene;
use crate::{engine, game, hud::markers};
use monaka_channel::ChannelName;
use monaka_hook::Hooks;
use monaka_hook::module::Module;
use monaka_producer::{Rejection, log};
use std::sync::atomic::{AtomicU64, Ordering};

/// Presents of the game's window (D3D11) or present requests (D3D12).
pub static PRESENTS: AtomicU64 = AtomicU64::new(0);
static PUBLISHED: AtomicU64 = AtomicU64::new(0);
static SKIPPED: AtomicU64 = AtomicU64::new(0);

/// Hooks `renderer`'s present and DLSS calls, D3D11's or D3D12's, and Streamline's imports; the
/// eyes go out on `channel`.
///
/// # Safety
/// `renderer` is the inspected build of the renderer the game loaded (`d3d11`: rd3d11).
pub unsafe fn install(hooks: &mut Hooks, renderer: &Module, d3d11: bool, channel: &ChannelName) -> Result<(), Rejection> {
    // SAFETY: the caller's guarantee (Streamline's imports are decoded, not assumed).
    unsafe { streamline::install(hooks, renderer) };
    if d3d11 {
        hooks.site(&render11::DLSS_EVALUATE, renderer, &engine::DLSS_EVALUATE_11, render11::dlss_evaluate)?;
        hooks.site(&render11::DLSS_CONSTANTS, renderer, &engine::DLSS_CONSTANTS_11, render11::dlss_constants)?;
        hooks.site(&render11::COMMANDS, renderer, &engine::COMMANDS_11, render11::commands)?;
        // SAFETY: DXGI's Present, found on a probe swapchain.
        unsafe { render11::install_present(hooks)? };
        render11::set_channel(channel.clone());
    } else {
        hooks.site(&render12::PRESENT_REQUEST, renderer, &engine::PRESENT_REQUEST_12, render12::present_request)?;
        hooks.site(&render12::DLSS_EVALUATE, renderer, &engine::DLSS_EVALUATE_12, render12::dlss_evaluate)?;
        hooks.site(&render12::DLSS_CONSTANTS, renderer, &engine::DLSS_CONSTANTS_12, render12::dlss_constants)?;
        render12::set_channel(channel.clone());
    }
    Ok(())
}

/// One publish attempt: counted, and every 600 presents the run's counters are logged.
pub fn count(ok: bool) {
    if ok { &PUBLISHED } else { &SKIPPED }.fetch_add(1, Ordering::Relaxed);
    let n = PRESENTS.load(Ordering::Relaxed);
    if n == 1 || n.is_multiple_of(600) {
        report();
    }
}

/// The run's counters: presents, pairs, the renderer's, DLSS's, Streamline's, frame generation's.
pub fn report() {
    log!(
        "presents={} published={} skipped={} stereo={} paused={}",
        PRESENTS.load(Ordering::Relaxed),
        PUBLISHED.load(Ordering::Relaxed),
        SKIPPED.load(Ordering::Relaxed),
        scene::STEREO_PAIRS.load(Ordering::Relaxed),
        scene::REJECTED_PAIRS.load(Ordering::Relaxed)
    );
    scene::report();
    let renderer = if game::try_get().is_some_and(|g| g.renderer11.is_some()) { render11::report() } else { render12::report() };
    log!(
        "DLSS eye calls={} resets={} matrix failures={}; {renderer}; {}; {}",
        dlss::EYE_CALLS.load(Ordering::Relaxed),
        dlss::RESETS.load(Ordering::Relaxed),
        dlss::MATRIX_FAILURES.load(Ordering::Relaxed),
        streamline::report(),
        framegen::report()
    );
    log!("{}", scene::timing_report());
    markers::report();
}
