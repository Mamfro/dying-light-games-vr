//! Dead Island Riptide Definitive Edition in head-tracked stereo: alternate-eye rendering in place,
//! published to the viewer through the D3D11 keyed-mutex channel. Injected by `monaka_loader`;
//! options come from `riptide-options.txt` (`config`) and the channel from `live-channel.txt`
//! beside the DLL copy.
//!
//! The game runs on Dying Light 1's engine (Chrome Engine 6, built in 2016 with an older
//! compiler), so this adapter is Dying Light 1's, cut down to what carries over: the engine's
//! layouts are the same, its addresses are this build's own (`providers::engine`). The arms,
//! the eye size switch and Dying Light 1's gameplay hooks are not here: Riptide's arms visual has
//! no camera-target callback to pose them at, and its input runs on Dead Island's own system.
//!
//! The adapter ([`Riptide::ADAPTER`]), in the order its components start, by feature: stereo (the
//! present loop, the eyes, AFW), the HUD (the back buffer's draws, the HUD in front of the eyes),
//! look (head aim with the mod-owned facing) and the pad.

mod config;
#[cfg(test)]
mod launch_tests;
mod manifest;
mod providers;

use config::Config;
use monaka_core::manifest::{AIM_HAND, AIM_LOOK, AIM_MOUSE, AIM_POINT, CONTROLLER_PAD, CONTROLLER_PROXY, Manifest, aim, aims};
use monaka_core::options::Options;
use monaka_hook::module::{self, Module};
use monaka_hook::mem;
use monaka_producer::game::{Adapter, Component, Game, Start};
use monaka_producer::{Rejection, Session, log, require_build};
use providers::engine;
use std::ffi::c_void;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;
use windows::core::{IUnknown, Interface};

monaka_producer::producer!(game: crate::Riptide);

/// Dead Island Riptide Definitive Edition, as Monaka runs it.
pub struct Riptide;

/// This build's modules: the engine, the D3D11 renderer (its swapchain global) and the game DLL.
pub struct Facts {
    pub engine: Module,
    pub renderer: Module,
    pub gamedll: Module,
}

impl Game for Riptide {
    const MANIFESTS: &'static [Manifest] = manifest::MANIFESTS;
    const ADAPTER: Adapter<Riptide> = Adapter { components: &[&PRESENT, &EYES, &AFW, &DRAWS, &HUD, &LOOK, &GAMEPAD] };
    type Build = Facts;
    type Frame = ();

    fn prepare(_: &'static Session, manifest: &'static Manifest, options: &Options) -> Result<Facts, Rejection> {
        let engine_module = require_build(eng_chr::ENGINE, engine::ENGINE_SHA256)?;
        let renderer = require_build(engine::RENDERER, engine::RENDERER_SHA256)?;
        let gamedll = require_build(engine::GAMEDLL, engine::GAMEDLL_SHA256)?;
        log!("{:?}", Config::from_options(options, manifest));
        Ok(Facts { engine: engine_module, renderer, gamedll })
    }
}

/// The run's settings ([`Config`]): one reader, called by every component that needs one.
fn config(start: &Start<'_, Riptide>) -> Config {
    Config::from_options(start.options, start.manifest)
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

// ---- Stereo ----

/// Each frame to the viewer by Monaka's present loop, with Riptide's steps and its eye site
/// (`Presents for Riptide`, [`providers::present_entry`]).
pub const PRESENT: Component<Riptide> = monaka_stereo::feature::present();

/// Stereo by alternate eyes, in place ([`providers::eye_site`], installed with the present loop):
/// the view setup moves the cached render camera to the eye the present loop chose, the game asked
/// for a field of view wide enough for both eyes; with pointing aim, the controllers' block is read
/// at each present. No `stereo` switch: the present loop's steps are the eyes'.
pub const EYES: Component<Riptide> = Component {
    reports: &[monaka_core::situation::Situation::WorldHidden],
    options: &[config::STEREO, monaka_core::manifest::HEAD_MOVEMENT, monaka_core::manifest::LATENCY, monaka_core::manifest::EYE_SEPARATION],
    switch: None,
    needs: &[&PRESENT],
    settings: |options, manifest| {
        Config::from_options(options, manifest);
    },
    release: providers::present_entry::release,
    ..monaka_stereo::eye_site::eyes()
};

/// AFW (Frame-warp, `stereo=afw`): alternate-eye rendering with depth, a pair every frame, the other
/// eye reprojected from the real one (`monaka_stereo::afw11`, on [`providers::depth`]); the
/// HUD a layer over both eyes.
pub const AFW: Component<Riptide> = Component {
    needs: &[&EYES],
    on: |run| monaka_stereo::afw::on(monaka_core::rendering::StereoMethod::chosen(&config::STEREO, run.options)),
    ..monaka_stereo::afw11::frame_warping()
};

// ---- The HUD ----

/// The back buffer's draws tracked on the immediate context ([`providers::hud_draws`]): for the
/// HUD and AFW's scene copy.
pub const DRAWS: Component<Riptide> = Component {
    name: "draws",
    needs: &[&PRESENT],
    install: providers::hud_draws::install_draws,
    ..Component::NONE
};

/// The HUD and menus in front of the eyes (`hud=0`: as the game draws them): the draws after the
/// world's get a viewport at the HUD's place in each eye ([`providers::hud_draws`], by the frame
/// labels). On the HUD layer's base (`monaka_stereo::hud_layer`).
pub const HUD: Component<Riptide> = Component { options: &monaka_stereo::hud_layer::options(0.36), needs: &[&DRAWS], ..monaka_stereo::hud_layer::hud_layer() };

/// The controllers as the game's pad, the whole right stick to the game (head aim owns the facing).
pub const GAMEPAD: Component<Riptide> = Component {
    options: &[CONTROLLER_PAD, CONTROLLER_PROXY],
    needs: &[&PRESENT],
    arm: monaka_pad::feature::whole_stick,
    ..monaka_pad::feature::gamepad()
};

// ---- Look ----

/// What aims: no hand rig in this game yet, so the head or a pointing controller (or neither).
const AIMS: [monaka_core::manifest::Opt; 2] = [aim(&[AIM_LOOK, AIM_POINT, AIM_MOUSE], aims::HEAD), AIM_HAND];

/// Head aim with the mod-owned facing (`monaka_body::look`, its facing delivery on Riptide's look
/// fields: [`providers::look_fields`]): the view faces where the player turned it, plus the head;
/// the character is steered there. Reports the cutscenes that take the camera.
pub const LOOK: Component<Riptide> = Component { options: &AIMS, needs: &[&EYES], ..monaka_body::look::look() };
