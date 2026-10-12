//! Provider: the present entry, where each eye's frame leaves the game, for the same-frame loop
//! (`monaka_stereo::same_frame`, its loop [`PresentsPairs for Dl2`](PresentsPairs)), with the
//! game's queue.
//!
//! - D3D11 (rd3d11, the game's default): the engine queues its present as a command; the pair's
//!   drain dispatches it at once, and the command dispatcher ([`commands`]) hands the eye's present
//!   to the loop, which copies the eye out of the back buffer just before that present runs.
//! - D3D12 (rd3d12): each present request ([`present_request`]) hands the frame to the loop; depth
//!   stereo's centre frame goes to its own method (`crate::extras::depth_stereo`).

use crate::{Dl2, Facts};
use crate::config::{self, debug};
use crate::providers::engine::{self, CommandsFn, PresentRequest12Fn};
use crate::providers::eye_site::{self, local};
use crate::providers::game;
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::game::{Lack, Start};
use monaka_stereo::providers::FrameLabels;
use monaka_stereo::same_frame::{Output, PAIRS, PairLoop, PresentsPairs, d3d11, d3d12};
use std::ffi::c_void;
use windows::Win32::Graphics::Direct3D12::{ID3D12CommandQueue, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain3;
use windows::core::Interface;

static COMMANDS: Original<CommandsFn> = Original::new();
static PRESENT_REQUEST: Original<PresentRequest12Fn> = Original::new();

/// Hooks the present entry of the renderer the game loaded: D3D11's command dispatcher or D3D12's
/// present request.
fn entry(start: &mut Start<'_, Dl2>) -> Result<(), Lack> {
    let Facts { renderer, d3d11, .. } = start.build;
    if *d3d11 {
        start.hooks.site(&COMMANDS, renderer, &engine::COMMANDS_11, commands)?;
    } else {
        start.hooks.site(&PRESENT_REQUEST, renderer, &engine::PRESENT_REQUEST_12, present_request)?;
    }
    Ok(())
}

/// DL2's pairs go out by the same-frame loop: each eye's frame labelled by the eye being
/// dispatched (with its pair's number and pose), published at the headset's eye size once the game
/// presents at it, the HUD layer laid over each eye (`crate::providers::hud`), the left eye's
/// present kept off the monitor (unless a check shows it); a pause starts each eye's history
/// afresh, and the `MONO` check holds every frame mono.
impl PresentsPairs for Dl2 {
    const LOOP: PairLoop<Self> = PairLoop {
        presenting: Some(<Dl2 as FrameLabels>::presenting),
        pair: Some(|| local(|l| l.dispatch_pair.get())),
        ready: crate::providers::video::at_headset_size,
        queue: Some(game_queue),
        synthetic: Some(crate::providers::head::synthetic),
        synthetic_records: true,
        hud: Some(crate::providers::hud::PAIR_HUD),
        hide_left: || !config::debug(debug::SHOW_LEFT),
        rate: Some(eye_site::rate),
        paused: Some(eye_site::paused),
        mono: || config::debug(debug::MONO),
        ..PairLoop::new(output, entry)
    };
}

fn output(start: &Start<'_, Dl2>) -> Output {
    if start.build.d3d11 { Output::D3D11 } else { Output::D3D12 }
}

/// The game's direct queue (a Streamline proxy when Streamline is loaded).
pub fn game_queue() -> Option<ID3D12CommandQueue> {
    engine::RD3D12.game_queue(game::get().renderer12?)
}

/// The renderer's queue drain, and the driver vtable slot that must name it.
pub fn drain_available(r11: usize) -> bool {
    let (rva, bytes) = engine::DRAIN_11;
    let mut actual = vec![0u8; bytes.len()];
    mem::read_bytes(r11 + rva, &mut actual)
        && actual == bytes
        && mem::read::<usize>(r11 + engine::DRIVER_VTABLE_11 + engine::DRIVER_DRAIN_SLOT_11) == Some(r11 + rva)
}

/// A command buffer holding only a present (type 51): every earlier draw of the frame has been
/// dispatched. Returns its swapchain.
fn pure_present_swap(object: usize) -> Option<usize> {
    let command = mem::read::<usize>(object).filter(|&c| c != 0)?;
    let [size, kind, ..] = mem::read::<[u32; 6]>(command)?;
    if size != 16 || kind != 51 || mem::read::<u32>(command + 20)? != 0 {
        return None;
    }
    let owner = mem::read::<usize>(command + 8).filter(|&o| o != 0)?;
    mem::read::<usize>(owner + 0x120).filter(|&s| s != 0)
}

/// The D3D11 command dispatcher: an eye's present, dispatched by the pair's drain, goes to the loop
/// first.
unsafe extern "system" fn commands(object: usize) -> usize {
    let _guard = InFlight::enter();
    // SAFETY (both calls): the original, with the renderer's argument.
    let original = || unsafe { COMMANDS.get()(object) };
    let eye = local(|l| l.dispatch_eye.get());
    if eye != 0
        && let Some(swap) = pure_present_swap(object)
    {
        let (copied, result) = d3d11::around_eye_present(swap, original);
        if copied {
            eye_site::eyes_taken(1);
        }
        return result;
    }
    original()
}

/// The D3D12 present request: the frame to the loop (depth stereo's centre frame to its own
/// method), and the HUD layer's frame ended.
unsafe extern "system" fn present_request(object: usize) -> usize {
    let _guard = InFlight::enter();
    if PAIRS.publishing() {
        let swap = engine::RD3D12.swapchain(object);
        if swap != 0 && engine::RD3D12.active(object) {
            let raw = swap as *mut c_void;
            // SAFETY: the request's swapchain, live while it presents.
            if let Some(swap) = unsafe { IDXGISwapChain3::from_raw_borrowed(&raw) }
                && d3d12::present(swap, own)
            {
                eye_site::eyes_taken(1);
            }
        }
        // The HUD layer's frame (its draws recorded on the game's lists) ends with the present.
        eng_chr::providers::hudlayer::frame_end();
    }
    // SAFETY: the original, with the game's argument.
    unsafe { PRESENT_REQUEST.get()(object) }
}

/// A frame DL2 publishes its own way (`buffer` its back buffer): depth stereo's centre frame, and a
/// check's mono frame.
fn own(buffer: &ID3D12Resource) -> Option<bool> {
    match local(|l| l.dispatch_eye.get()) {
        3 => Some(crate::extras::depth_stereo::present(buffer)),
        0 => crate::extras::depth_stereo::hudless_left(buffer),
        _ => None,
    }
}
