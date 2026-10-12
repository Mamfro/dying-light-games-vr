//! Dying Light 1 in head-tracked stereo: alternate-eye rendering in place, published to the
//! viewer through the D3D11 keyed-mutex channel. Injected by `monaka_loader`; options come from
//! `dl1-options.txt` (`config`) and the channel from `live-channel.txt` beside the DLL copy.
//!
//! Game requirements: NVIDIA HBAO+, depth of field and PCSS off (they get the wrong camera and
//! make shadows slide), and no `/3dtv` launch option.
//!
//! The adapter ([`Dl1::ADAPTER`]), in the order its components start, by feature: [`stereo`] (the
//! eye size first, since the channel, the HUD and the upscaler take the back buffer's size; the
//! present loop, the eyes, FSR, the depth experiment), the [`hud`] (the back buffer's draws, the
//! HUD in front of the eyes, its pieces on the hands, the UI kept at the monitor's size), [`look`]
//! (head aim with the mod-owned facing), [`arms`] (the hand rig, fingers, and what the hands and
//! steps do), the [`controls`], [`grabs`], the [`pad`] and the probes.
//!
//! `flat=1` (research) runs the probes alone on the game as it plays on the monitor: everything
//! that needs stereo is off; `flat_buttons=1` keeps the buttons and grabs.

mod arms;
mod config;
mod controls;
mod engine;
mod grabs;
mod hud;
mod look;
mod manifest;
#[cfg(test)]
mod launch_tests;
mod pad;
mod research;
mod stereo;

use config::Config;
use monaka_core::manifest::Manifest;
use monaka_core::options::Options;
use monaka_hook::module::{self, Module};
use monaka_hook::{Hooks, mem};
use monaka_producer::game::{Adapter, Component, Game, Lack, On, Run, Start};
use monaka_producer::{Rejection, Session, log, require_build};
use std::ffi::c_void;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;
use windows::core::{IUnknown, Interface};

monaka_producer::producer!(game: crate::Dl1);

/// Dying Light 1, as Monaka runs it.
pub struct Dl1;

/// This build's modules: the engine, the D3D11 renderer (its swapchain global) and the game DLL.
pub struct Facts {
    pub engine: Module,
    pub renderer: Module,
    pub gamedll: Module,
}

impl Game for Dl1 {
    const MANIFESTS: &'static [Manifest] = manifest::MANIFESTS;
    const ADAPTER: Adapter<Dl1> = Adapter {
        components: &[
            &stereo::EYE_SIZE,
            &stereo::PRESENT,
            &stereo::EYES,
            &stereo::UPSCALE,
            &stereo::HYBRID,
            &hud::DRAWS,
            &hud::HUD,
            &hud::PANELS,
            &hud::UI_SIZE,
            &look::LOOK,
            &arms::ARMS,
            &arms::FINGERS,
            &arms::MELEE,
            &arms::THROW,
            &arms::BOW,
            &arms::GRAPPLE,
            &arms::LOCKPICK,
            &arms::GUN,
            &arms::ROOM_SCALE,
            &controls::CONTROLS,
            &grabs::GRABS,
            &pad::GAMEPAD,
            &PROBES,
        ],
    };
    type Build = Facts;
    type Frame = ();

    fn prepare(session: &'static Session, manifest: &'static Manifest, options: &Options) -> Result<Facts, Rejection> {
        let engine_module = require_build(engine::ENGINE, engine::ENGINE_SHA256)?;
        let renderer = require_build(engine::RENDERER, engine::RENDERER_SHA256)?;
        let gamedll = require_build(engine::GAMEDLL, engine::GAMEDLL_SHA256)?;
        log!("{:?}", Config::from_options(options, manifest));
        research::configure(options, session.dir());
        Ok(Facts { engine: engine_module, renderer, gamedll })
    }
}

/// The run's settings ([`Config`]): one reader, called by every component that needs one.
fn config(start: &Start<'_, Dl1>) -> Config {
    Config::from_options(start.options, start.manifest)
}

/// Stereo runs (`flat=1`: the probes alone).
fn stereo_on(_: &Run<'_, Dl1>) -> On {
    On::when(!research::options().flat, "flat: the probes only, no stereo")
}

/// What runs without a headset in a flat run too (`flat_buttons=1`).
fn flat_kept() -> bool {
    !research::options().flat || research::options().flat_buttons
}

/// The game's swapchain, from the renderer global, checked before any call into it.
fn game_swapchain(renderer: &Module) -> Result<IDXGISwapChain, Rejection> {
    let candidate = mem::read::<usize>(renderer.at(engine::SWAPCHAIN_GLOBAL))
        .filter(|&object| module::plausible_object(object, 3))
        .ok_or_else(|| Rejection::not_ready("the renderer holds no swapchain yet"))?;
    let raw = candidate as *mut c_void;
    // SAFETY: a pointer whose vtable lies in loaded code; only QueryInterface is called on it.
    let unknown = unsafe { IUnknown::from_raw_borrowed(&raw) }.ok_or_else(|| Rejection::not_ready("no swapchain"))?;
    unknown.cast::<IDXGISwapChain>().map_err(|e| Rejection::not_ready(format!("the renderer's object is not a swapchain: {e}")))
}

/// The game's swapchain, its device and immediate context, and its size now.
fn swapchain_parts(renderer: &Module) -> Result<(IDXGISwapChain, ID3D11Device, ID3D11DeviceContext, (u32, u32)), Rejection> {
    let swap = game_swapchain(renderer)?;
    // SAFETY: COM calls on the game's live swapchain and its device.
    unsafe {
        let device: ID3D11Device = swap.GetDevice().map_err(|e| Rejection::not_ready(format!("swapchain device: {e}")))?;
        let context = device.GetImmediateContext().map_err(|e| Rejection::not_ready(format!("immediate context: {e}")))?;
        let desc = swap.GetDesc().map_err(|e| Rejection::not_ready(format!("swapchain description: {e}")))?;
        Ok((swap, device, context, (desc.BufferDesc.Width, desc.BufferDesc.Height)))
    }
}

/// [`Hooks::method`] (in the code, the vtable slot as a fallback), logging a fallback.
///
/// # Safety
/// `object` must be a live COM object whose method `index` has type `F`.
unsafe fn hook_method<F: Copy>(hooks: &mut Hooks, original: &monaka_hook::Original<F>, name: &str, object: *mut c_void, index: usize, detour: F) -> Result<(), Lack> {
    // SAFETY: the caller's guarantee.
    unsafe { hooks.method_noted(original, name, object, index, detour, |why| log!("{why}; hooking its vtable slot instead")) }?;
    Ok(())
}

/// The research probes: their hooks, each checking its own option; the look and arms probes take
/// their component's hook when it is off.
pub const PROBES: Component<Dl1> = Component { install: research::install_probes, quiet: research::report, ..monaka_producer::game::probes() };
