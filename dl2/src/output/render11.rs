//! The D3D11 renderer side (rd3d11, the game's default): same-frame stereo over the keyed-mutex
//! channel.
//!
//! The engine queues its present as a command; the pair's drain dispatches it at once, and the
//! command dispatcher copies the eye out of the back buffer just before that present runs. The left
//! eye's present is kept off the monitor. Without a recent pair (menus, loading, no head pose) the
//! frame goes out mono at DXGI's Present.
//!
//! While VR runs the game renders at the headset's eye size when asked (`crate::output::video`).

use crate::config::{self, debug};
use crate::engine::{self, CommandsFn, PacketFn};
use crate::view::scene::{self, LAST_STEREO_TICK, local};
use crate::{output::dlss, game, view::head};
use monaka_channel::ChannelName;
use monaka_stereo::pairs11::{PairPublisher11, Step};
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::Rejection;
use monaka_stereo::present::PresentFn;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use windows::Win32::Foundation::S_OK;
use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::{DXGI_PRESENT_TEST, IDXGISwapChain};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows::core::{HRESULT, Interface};

pub static DLSS_EVALUATE: Original<PacketFn> = Original::new();
pub static DLSS_CONSTANTS: Original<PacketFn> = Original::new();
pub static COMMANDS: Original<CommandsFn> = Original::new();
static DXGI_PRESENT: Original<PresentFn> = Original::new();

static CHANNEL: Mutex<Option<PairPublisher11>> = Mutex::new(None);
/// The game is recognised by its window: a mode change can give it a new swapchain.
static GAME_WINDOW: AtomicUsize = AtomicUsize::new(0);
static ALL_PRESENTS: AtomicU64 = AtomicU64::new(0);
static PRESENTS_HIDDEN: AtomicU64 = AtomicU64::new(0);

pub fn set_channel(name: ChannelName) {
    *CHANNEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(PairPublisher11::new(name));
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

/// Whether the game's window has focus (it applies video changes only then).
fn focused() -> bool {
    // SAFETY: plain query.
    unsafe { GetForegroundWindow() }.0 as usize == GAME_WINDOW.load(Ordering::Relaxed)
}

// --- Capture ----------------------------------------------------------------------------------------
/// Publishing stopped for good (the channel failed).
fn broken() -> bool {
    CHANNEL.lock().ok().is_some_and(|c| c.as_ref().is_some_and(PairPublisher11::broken))
}

/// The back buffer, its device and immediate context, and its description.
fn back_buffer(swap: &IDXGISwapChain) -> Option<(ID3D11Texture2D, ID3D11Device, ID3D11DeviceContext, D3D11_TEXTURE2D_DESC)> {
    let (buffer, device, context) = monaka_channel::d3d::swapchain_parts(swap).ok()?;
    let desc = monaka_channel::d3d::texture_desc(&buffer);
    Some((buffer, device, context, desc))
}

fn published(step: Step) {
    crate::count(step == Step::Published);
}

/// Copies one eye out of the back buffer before its present runs; after the right eye the pair is
/// published. The channel opens at the first frame at the headset's size when that was asked for
/// (the viewer keeps the first size).
fn capture_eye(swap: usize, eye: u32, pair: u64) {
    let raw = swap as *mut c_void;
    // SAFETY: the swapchain of the present command being dispatched.
    let Some(swap) = (unsafe { IDXGISwapChain::from_raw_borrowed(&raw) }) else { return };
    let Some((buffer, device, context, _)) = back_buffer(swap) else { return };
    let mut guard = CHANNEL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(publisher) = guard.as_mut().filter(|p| !p.broken()) else { return };
    if !publisher.copy_eye(&device, &context, (eye - 1) as usize, &buffer, pair, crate::output::video::at_headset_size) {
        return;
    }
    local(|l| l.eye_copies.set(l.eye_copies.get() + 1));
    if eye != 2 || !publisher.complete(pair) {
        return;
    }
    let pair_head = local(|l| l.pair_head.get());
    published(publisher.publish_pair(&context, Some([pair_head, pair_head]), &head::HEAD, game::tick()));
}

pub unsafe extern "system" fn commands(object: usize) -> usize {
    let _guard = InFlight::enter();
    let eye = local(|l| l.dispatch_eye.get());
    if eye != 0
        && let Some(swap) = pure_present_swap(object)
    {
        local(|l| l.dispatched_swap.set(swap));
        capture_eye(swap, eye, local(|l| l.dispatch_pair.get()));
    }
    // SAFETY: the original, with the renderer's argument.
    let result = unsafe { COMMANDS.get()(object) };
    local(|l| l.dispatched_swap.set(0));
    result
}

unsafe extern "system" fn dxgi_present(swap: *mut c_void, interval: u32, flags: u32) -> HRESULT {
    let _guard = InFlight::enter();
    if scene::publishing() && flags & DXGI_PRESENT_TEST.0 == 0 {
        ALL_PRESENTS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: DXGI's own swapchain argument.
        if let Some(chain) = unsafe { IDXGISwapChain::from_raw_borrowed(&swap) } {
            // SAFETY: COM calls on a live swapchain.
            let window = unsafe { chain.GetDesc() }.map_or(0, |d| d.OutputWindow.0 as usize);
            if GAME_WINDOW.load(Ordering::Relaxed) == 0 && window != 0 && unsafe { chain.GetDevice::<ID3D11Device>() }.is_ok() {
                GAME_WINDOW.store(window, Ordering::Relaxed);
            }
            if window != 0 && window == GAME_WINDOW.load(Ordering::Relaxed) {
                crate::PRESENTS.fetch_add(1, Ordering::Relaxed);
                let (dispatched, eye) = local(|l| (l.dispatched_swap.get(), l.dispatch_eye.get()));
                // The left eye's present is not shown on the monitor; the right eye's is.
                if swap as usize == dispatched && eye == 1 && !config::debug(debug::SHOW_LEFT) {
                    PRESENTS_HIDDEN.fetch_add(1, Ordering::Relaxed);
                    return S_OK;
                }
                // Without a recent stereo pair (menus, loading, no head pose), the frame goes out mono.
                if eye == 0 && game::tick().saturating_sub(LAST_STEREO_TICK.load(Ordering::Acquire)) > 250 && !broken() {
                    publish_mono(chain);
                }
            }
        }
    }
    // SAFETY: the original, with DXGI's arguments.
    unsafe { DXGI_PRESENT.get()(swap, interval, flags) }
}

fn publish_mono(swap: &IDXGISwapChain) {
    let Some((buffer, device, context, _)) = back_buffer(swap) else { return };
    let mut guard = CHANNEL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(publisher) = guard.as_mut() {
        published(publisher.publish_flat(&device, &context, &buffer, game::tick(), crate::output::video::at_headset_size));
    }
}

/// DLSS runs inside the drain on the pair's thread; each eye gets its own feature and matrices.
unsafe fn scoped(packet: usize, original: PacketFn, evaluate: bool) -> usize {
    let _guard = InFlight::enter();
    let dispatch = local(|l| l.dispatch_eye.get());
    let eye = if config::debug(debug::NATIVE_DLSS_HISTORY) || dispatch > 2 { 0 } else { dispatch };
    let Some(r11) = game::get().renderer11.filter(|_| eye != 0) else {
        // SAFETY: the original, with the renderer's argument.
        return unsafe { original(packet) };
    };
    // SAFETY: the renderer's DLSS viewport id (a u32), put back after the call; the packet is the
    // renderer's constants packet; DLSS runs on the pair's thread alone.
    unsafe { dlss::for_eye(eye, (r11 + engine::DLSS_VIEWPORT_11) as *mut u32, None, packet, evaluate, |packet| original(packet)) }
}

pub unsafe extern "system" fn dlss_evaluate(packet: usize) -> usize {
    // SAFETY: forwards the renderer's argument.
    unsafe { scoped(packet, DLSS_EVALUATE.get(), true) }
}

pub unsafe extern "system" fn dlss_constants(packet: usize) -> usize {
    // SAFETY: forwards the renderer's argument.
    unsafe { scoped(packet, DLSS_CONSTANTS.get(), false) }
}

/// Hooks DXGI's Present (`monaka_stereo::present`).
///
/// # Safety
/// Called once at start, before the hooks are enabled.
pub unsafe fn install_present(hooks: &mut Hooks) -> Result<(), Rejection> {
    // SAFETY: the caller's guarantee; `dxgi_present` forwards to the original.
    unsafe { monaka_stereo::present::hook_dxgi_present(hooks, &DXGI_PRESENT, dxgi_present) }
}

pub fn close() {
    CHANNEL.lock().unwrap_or_else(|e| e.into_inner()).take();
}

pub fn report() -> String {
    format!(
        "left presents hidden={}; game window focused={}; presents on any swapchain={}",
        PRESENTS_HIDDEN.load(Ordering::Relaxed),
        focused(),
        ALL_PRESENTS.load(Ordering::Relaxed)
    )
}
