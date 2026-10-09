//! The D3D12 present request: how often the game presents, on its first present the swapchain and
//! the game's queue checked by interface queries (the swapchain may be a Streamline proxy), and
//! with stereo on, the hand-over to [`crate::view::stereo`] around each present.

use crate::engine::{self, PresentRequest12Fn};
use monaka_hook::{InFlight, Original};
use monaka_producer::log;
use eng_chr::rd3d12::query;
use std::sync::atomic::Ordering::{AcqRel, Relaxed};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue;
use windows::Win32::Graphics::Dxgi::IDXGISwapChain3;
use windows::core::Interface;

pub static PRESENT_REQUEST: Original<PresentRequest12Fn> = Original::new();
static RENDERER_12: AtomicUsize = AtomicUsize::new(0);
static REQUESTS: AtomicU64 = AtomicU64::new(0);
static PRESENTS: AtomicU64 = AtomicU64::new(0);
static REPORTED: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static DESCRIBED: AtomicBool = AtomicBool::new(false);

pub fn set_renderer(base: usize) {
    RENDERER_12.store(base, Relaxed);
}

/// The present request: counted, and with stereo on, this frame's eye kept or its pair published
/// before the game presents it, and the next eye chosen after.
pub unsafe extern "system" fn present_request(object: usize) -> usize {
    let _flight = InFlight::enter();
    REQUESTS.fetch_add(1, Relaxed);
    let mut n = None;
    if engine::RD3D12.active(object) {
        PRESENTS.fetch_add(1, Relaxed);
        if !DESCRIBED.swap(true, AcqRel) {
            describe(object);
        }
        let swap = query::<IDXGISwapChain3>(engine::RD3D12.swapchain(object));
        // SAFETY: a plain query of the live swapchain on the thread that presents it.
        let size = swap.as_ref().and_then(|s| unsafe { s.GetDesc1() }.ok()).map(|desc| (desc.Width, desc.Height));
        if let Some((width, height)) = size {
            crate::output::video::presented(width, height);
        }
        // The HUD layer of this frame: its pieces onto the hands and out of the eye about to be
        // taken (the back buffer, in PRESENT).
        let queue = game_queue();
        let layer = queue.as_ref().zip(size).and_then(|(queue, size)| eng_chr::hudlayer::take_layer(queue, size));
        if let (Some(queue), Some(layer), Some(swap), true) = (&queue, &layer, &swap, crate::WORLD_HUD.load(Relaxed))
            && let Some(channel) = crate::view::stereo::channel()
        {
            let rects = eng_chr::panels::publish(queue, layer, &channel, monaka_channel::tick());
            // SAFETY: COM calls on the game's live swapchain, on the thread that presents it.
            if let Ok(buffer) = unsafe { swap.GetBuffer::<windows::Win32::Graphics::Direct3D12::ID3D12Resource>(swap.GetCurrentBackBufferIndex()) } {
                eng_chr::hudfix::take_out(queue, &buffer, windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATE_PRESENT, layer, &rects);
            }
        }
        n = swap.and_then(|swap| crate::view::stereo::before_present(&swap, queue.clone()));
        drop(layer);
        if let Some(queue) = &queue {
            eng_chr::hudlayer::return_layer(queue);
        }
        eng_chr::hudlayer::frame_end();
    }
    // SAFETY: the original, with the game's argument.
    let result = unsafe { PRESENT_REQUEST.get()(object) };
    if let Some(n) = n {
        crate::view::stereo::after_present(n);
    }
    result
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
