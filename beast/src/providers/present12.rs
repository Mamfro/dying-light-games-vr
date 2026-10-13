//! Provider: the present entry (`monaka_stereo::feature::Presents`: where a frame leaves the game,
//! and the present loop's steps for The Beast), with the game's queue
//! (`monaka_stereo::providers::GameQueue`). The HUD layer's work at the present is Monaka's
//! lay-over (`monaka_stereo::hud_layer::present12`), run from here.
//!
//! The D3D12 present request: how often the game presents, on its first present the swapchain and
//! the game's queue checked by interface queries (the swapchain may be a Streamline proxy), the
//! lay-over (the features' work on the frame before its HUD, the HUD's pieces onto the hands, the
//! HUD over the frame and the features' images), and each present made inside Monaka's present
//! loop (`monaka_stereo::feature`'s `DRIVER`: the eye kept or its pair published before it, the
//! next eye chosen after it).

use crate::Beast;
use crate::providers::engine;
use crate::providers::eye_site;
use monaka_stereo::hud_layer::present12 as hud_layer;
use monaka_hook::{InFlight, Original};
use monaka_producer::game::{Lack, OrMissing, Start};
use monaka_producer::log;
use monaka_stereo::driver::Flat;
use monaka_stereo::feature::{Entry, Graphics, Loop, Presents};
use monaka_stereo::{afw12, framegen12};
use eng_chr::providers::rd3d12::{PresentRequestFn, query};
use std::sync::atomic::Ordering::{AcqRel, Relaxed};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use windows::Win32::Graphics::Direct3D12::{ID3D12CommandQueue, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::{IDXGISwapChain, IDXGISwapChain3};
use windows::core::Interface;

pub static PRESENT_REQUEST_ORIGINAL: Original<PresentRequestFn> = Original::new();
static RENDERER_12: AtomicUsize = AtomicUsize::new(0);
static REQUESTS: AtomicU64 = AtomicU64::new(0);
static PRESENTS: AtomicU64 = AtomicU64::new(0);
static REPORTED: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static DESCRIBED: AtomicBool = AtomicBool::new(false);

pub fn set_renderer(base: usize) {
    RENDERER_12.store(base, Relaxed);
}

/// The present request: counted, the HUD layer's work done, and the present made inside the
/// present loop.
pub unsafe extern "system" fn present_request(object: usize) -> usize {
    let _flight = InFlight::enter();
    REQUESTS.fetch_add(1, Relaxed);
    // SAFETY: the original, with the game's argument.
    let call = || unsafe { PRESENT_REQUEST_ORIGINAL.get()(object) };
    if !engine::RD3D12.active(object) {
        return call();
    }
    {
        PRESENTS.fetch_add(1, Relaxed);
        if !DESCRIBED.swap(true, AcqRel) {
            describe(object);
        }
        let swap = query::<IDXGISwapChain3>(engine::RD3D12.swapchain(object));
        // SAFETY: a plain query of the live swapchain on the thread that presents it.
        let size = swap.as_ref().and_then(|s| unsafe { s.GetDesc1() }.ok()).map(|desc| (desc.Width, desc.Height));
        // The HUD layer's work on this frame (Monaka's lay-over): in play the features' work on the
        // frame still without its HUD (frame warping, frame generation), the pieces onto the hands
        // (the dynamic HUD), and the eye about to be taken (the back buffer, in PRESENT) given its
        // HUD in front of it or, with the HUD drawn in, the pieces taken out. A menu is the game's
        // whole screen, shown flat: its frame keeps the menu as the game draws it.
        let running = crate::providers::eye_site::running();
        let queue = game_queue();
        // SAFETY: COM calls on the game's live swapchain, on the thread that presents it.
        let buffer = swap.as_ref().and_then(|swap| unsafe { swap.GetBuffer::<ID3D12Resource>(swap.GetCurrentBackBufferIndex()) }.ok());
        hud_layer::at_present::<Beast>(hud_layer::Present {
            queue: queue.as_ref(),
            buffer: buffer.as_ref(),
            size,
            menu: running && eng_chr::providers::gui::menu_open(),
            running,
        });
        match swap.and_then(|swap| swap.cast::<IDXGISwapChain>().ok()) {
            Some(chain) => monaka_stereo::feature::DRIVER.around_present(&chain, call),
            None => call(),
        }
    }
}

/// The game's direct queue (a Streamline proxy when Streamline is loaded).
pub fn game_queue() -> Option<ID3D12CommandQueue> {
    engine::RD3D12.game_queue(RENDERER_12.load(Relaxed))
}

fn describe(object: usize) {
    let swap_address = engine::RD3D12.swapchain(object);
    match query::<IDXGISwapChain3>(swap_address) {
        Some(swap) => {
            // SAFETY: plain queries of a live swapchain on the thread that presents it.
            let desc = unsafe { swap.GetDesc1() };
            match desc {
                Ok(d) => log!(
                    "swapchain {swap_address:#x}: {}x{} format {} buffers {} back buffer {}",
                    d.Width,
                    d.Height,
                    d.Format.0,
                    d.BufferCount,
                    // SAFETY: as above.
                    unsafe { swap.GetCurrentBackBufferIndex() }
                ),
                Err(e) => log!("swapchain {swap_address:#x}: GetDesc1 failed: {e}"),
            }
        }
        None => log!("present request +{:#x} holds {swap_address:#x}, not a swapchain", engine::RD3D12.request_swapchain),
    }
    match game_queue() {
        // SAFETY: a plain query of a live queue.
        Some(queue) => log!("game queue {:?}: type {}", queue.as_raw(), unsafe { queue.GetDesc() }.Type.0),
        None => log!("the queue holder at {:#x} holds no command queue", engine::RD3D12.queue_holder),
    }
}

/// Requests and presents per second since the last report.
pub fn report(seconds: f64) {
    let counts = [REQUESTS.load(Relaxed), PRESENTS.load(Relaxed)];
    let since = monaka_producer::rates(counts, &REPORTED, seconds);
    log!("present requests {:.1}/s, presents {:.1}/s", since[0], since[1]);
}

/// The present request component's install (`crate::PRESENT_REQUEST`): the D3D12 present request
/// hooked, the HUD laid over each frame and the present made inside the present loop
/// ([`present_request`]).
pub(crate) fn install_present(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let r12 = start.build.renderer_12.as_ref().or_missing("the game is not on D3D12")?;
    set_renderer(r12.base());
    start.hooks.site(&PRESENT_REQUEST_ORIGINAL, r12, &engine::PRESENT_REQUEST_12, present_request)?;
    Ok(())
}

/// The loop's steps for The Beast: frames labelled by their camera as DLSS gets it, the camera
/// updates taking the eye choices in turn, a menu flat, publishing held for the eye size, frame
/// generation's own pairs ([`eye_site`]).
impl Presents for Beast {
    const LOOP: Loop<Self> = Loop {
        entry: Entry::Game,
        queue: Some(|_| game_queue()),
        report: Some(monaka_stereo::eye_site::loop_report::<Beast>),
        eye_site: Some(<Beast as monaka_stereo::eye_site::EyeSite>::install),
        before: Some(eye_site::before_present),
        after: Some(eye_site::after_present),
        adjust_pose: Some(eye_site::latency_test),
        // Flat for a menu, and while no eye view has drawn the world for a few presents (a video,
        // a loading screen).
        flat: Some(|| eng_chr::providers::gui::menu_open().then_some(Flat::Menu).or_else(monaka_stereo::eye_site::world_hidden::<Beast>)),
        in_turn: true,
        pose_each_frame: Some(pose_each_frame),
        ..Loop::new(|_| Graphics::D3D12, ("eye cameras", &eye_site::CAMERAS.drawn))
    };
}

/// Each frame is drawn from the newest head pose: with frame warping (the pair is of that moment)
/// and per-eye frame generation (a generated eye stands for the other eye's moment).
fn pose_each_frame() -> bool {
    afw12::enabled() || framegen12::enabled()
}
