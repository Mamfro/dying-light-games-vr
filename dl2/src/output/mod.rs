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
    let at = |target: &engine::Target| renderer.at(target.0);
    // SAFETY: every target is checked against its exact prologue (or is decoded, for system and
    // Streamline code); every detour has its target's type.
    unsafe {
        streamline::install(hooks, renderer);
        if d3d11 {
            use engine::{COMMANDS_11, DLSS_CONSTANTS_11, DLSS_EVALUATE_11};
            hooks.inline(&render11::DLSS_EVALUATE, "DLSS evaluate", at(&DLSS_EVALUATE_11), DLSS_EVALUATE_11.1, render11::dlss_evaluate as engine::PacketFn)?;
            hooks.inline(&render11::DLSS_CONSTANTS, "DLSS constants", at(&DLSS_CONSTANTS_11), DLSS_CONSTANTS_11.1, render11::dlss_constants as engine::PacketFn)?;
            hooks.inline(&render11::COMMANDS, "command dispatcher", at(&COMMANDS_11), COMMANDS_11.1, render11::commands as engine::CommandsFn)?;
            render11::install_present(hooks)?;
            render11::set_channel(channel.clone());
        } else {
            use engine::{DLSS_CONSTANTS_12, DLSS_EVALUATE_12, PRESENT_REQUEST_12};
            hooks.inline(&render12::PRESENT_REQUEST, "rd3d12 present request", at(&PRESENT_REQUEST_12), PRESENT_REQUEST_12.1, render12::present_request as engine::PresentRequest12Fn)?;
            hooks.inline(&render12::DLSS_EVALUATE, "DLSS evaluate (D3D12)", at(&DLSS_EVALUATE_12), DLSS_EVALUATE_12.1, render12::dlss_evaluate as engine::Packet12Fn)?;
            hooks.inline(&render12::DLSS_CONSTANTS, "DLSS constants (D3D12)", at(&DLSS_CONSTANTS_12), DLSS_CONSTANTS_12.1, render12::dlss_constants as engine::Packet12Fn)?;
            render12::set_channel(channel.clone());
        }
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
