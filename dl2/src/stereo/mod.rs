//! Stereo in the mode the run picked (`config`). Where each eye's camera goes: the head pose, from
//! the viewer or synthetic on PC runs ([`head`]), the stereo pair on the engine side, each eye's
//! camera and frustum written into the scene ([`scene`]), and the eye camera maths in Dying Light
//! 2's camera form ([`camera`]). How finished images leave the game: the eye images taken from the
//! back buffer at each present request and published ([`render11`], [`render12`]), the depth warp
//! and the HUD lay-over ([`warp12`]), FSR frame generation per eye ([`framegen`]), per-eye DLSS and
//! its history ([`dlss`], [`temporal`]), the Streamline tags they read ([`streamline`]), and the
//! switch to the headset's eye size ([`video`]).

pub(crate) mod camera;
pub(crate) mod dlss;
pub(crate) mod framegen;
pub(crate) mod head;
pub(crate) mod render11;
pub(crate) mod render12;
pub(crate) mod scene;
pub(crate) mod streamline;
pub(crate) mod temporal;
pub(crate) mod video;
pub(crate) mod warp12;

use crate::config::{self, Config};
use crate::{Dl2, Facts, engine, game, hud::markers};
use monaka_channel::ChannelName;
use monaka_channel::pose::PoseChannel;
use monaka_core::feature::Feature;
use monaka_core::manifest::{Kind, Opt, Shown, opt, render_size};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight};
use monaka_producer::game::{Component, Lack, Start};
use monaka_producer::{Rejection, log};
use std::sync::atomic::{AtomicU64, Ordering};

const MODE: Opt = opt("mode", "Rendering", "Smooth renders each eye and adds in-between frames, Sharp renders each eye, Fast draws once for both eyes (gaps beside near things).", Kind::Choice {
    choices: &[("framegen", "Smooth"), ("same", "Sharp"), ("depth", "Fast"), ("alternate", "Alternate eye"), ("auto", "Smooth")],
    default: "framegen",
    window: 3,
})
.on(Shown::Engine)
// Fast shows the hands no nearer than its nearest comfortable depth: the panels on them too.
.viewer_args("depth", &["--panel-nearest", "0.5"]);

/// Stereo in the mode the run picked: the pair on the engine side, each eye's camera and frustum
/// written into the scene ([`scene`]), the head from the viewer or synthetic ([`head`]); the eyes
/// leave the renderer at its present ([`install`]), at the headset's eye size ([`video`]). Always
/// on: menus and loading go out mono.
pub const EYES: Component<Dl2> = Component {
    name: "eyes",
    features: &[Feature::Stereo, Feature::PerEyeHistory],
    options: &[render_size("Experimental in this game."), MODE],
    needs_channel: true,
    settings: |options, manifest| {
        Config::from_options(options, manifest);
    },
    install: install_eyes,
    arm: start_publishing,
    quiet: stop_publishing,
    give_back: give_back_eyes,
    release: release_eyes,
    summary: Some(scene::timing_report),
    ..Component::NONE
};

fn install_eyes(start: &mut Start<'_, Dl2>) -> Result<(), Lack> {
    let Facts { engine, renderer, d3d11, .. } = start.build;
    let channel = start.channel()?;
    // SAFETY: the engine's and the renderer's hashes were checked.
    unsafe {
        scene::install(start.hooks, engine)?;
        install(start.hooks, renderer, *d3d11, channel)?;
    }
    // A synthetic head still writes pose records (the viewer and the probe read them).
    head::open(PoseChannel::open(channel).map_err(|e| log!("pose channel: {e}; publishing mono")).ok());
    Ok(())
}

fn start_publishing() {
    scene::PUBLISHING.store(true, Ordering::Release);
    let config = config::get();
    let world = config.hud.world && game::try_get().is_some_and(|g| g.renderer12.is_some());
    let mode = match (config.framegen, config.depth_stereo, config.alternate_eye) {
        (true, _, _) => "same-frame stereo with FSR frame generation per eye",
        (_, true, _) => "depth stereo (one render, both eyes from depth)",
        (_, _, true) => "alternate-eye stereo",
        _ => "same-frame stereo",
    };
    let pose = if config.synthetic.is_some() {
        "a synthetic pose"
    } else if head::HEAD.connected() {
        "the viewer"
    } else {
        "nowhere (mono)"
    };
    log!("DL2 producer running: {mode}{}; head pose from {pose}", if world { " with the dynamic HUD" } else { "" });
}

/// A pair in progress restores the camera itself before returning.
fn stop_publishing() {
    scene::PUBLISHING.store(false, Ordering::Release);
    scene::stop_stereo();
}

fn give_back_eyes() {
    video::restore();
    InFlight::wait_idle(2000);
    scene::abandon_pair();
}

fn release_eyes() {
    framegen::close();
    render11::close();
    render12::close();
    head::HEAD.close();
    report();
}

/// Presents of the game's window (D3D11) or present requests (D3D12).
pub static PRESENTS: AtomicU64 = AtomicU64::new(0);
static PUBLISHED: AtomicU64 = AtomicU64::new(0);
static SKIPPED: AtomicU64 = AtomicU64::new(0);

/// Hooks `renderer`'s present and DLSS calls, D3D11's or D3D12's, and Streamline's imports; the
/// eyes go out on `channel`.
///
/// # Safety
/// `renderer` is the inspected build of the renderer the game loaded (`d3d11`: rd3d11).
unsafe fn install(hooks: &mut Hooks, renderer: &Module, d3d11: bool, channel: &ChannelName) -> Result<(), Rejection> {
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
