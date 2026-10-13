//! Dying Light 1 in head-tracked stereo: alternate-eye rendering in place, published to the
//! viewer through the D3D11 keyed-mutex channel. Injected by `monaka_loader`; options come from
//! `dl1-options.txt` (`config`) and the channel from `live-channel.txt` beside the DLL copy.
//!
//! Game requirements: NVIDIA HBAO+, depth of field and PCSS off (they get the wrong camera and
//! make shadows slide), and no `/3dtv` launch option.
//!
//! The adapter ([`Dl1::ADAPTER`]), in the order its components start, by feature: stereo (the eye
//! size first, since the channel, the HUD and the upscaler take the back buffer's size; the present
//! loop, the eyes, FSR, AFW), the HUD (the back buffer's draws, the HUD in front of the eyes, its
//! pieces on the hands, the UI kept at the monitor's size), look (head aim with the mod-owned
//! facing, the pitch lock), the arms (the hand rig, fingers, and what the hands and steps do), the
//! VR buttons, grabs, the pad and the probes.
//!
//! The code is sorted by who should own it: [`providers`] (what only Dying Light 1 knows: its
//! facts, its hooks, reading and writing the game), [`extras`] (features only this game has) and
//! [`research`] (the probes); it has no cooking left. The components are listed here, the extras'
//! beside their features.
//!
//! `flat=1` (research) runs the probes alone on the game as it plays on the monitor: everything
//! that needs stereo is off; `flat_buttons=1` keeps the buttons and grabs.

mod config;
mod extras;
mod manifest;
#[cfg(test)]
mod launch_tests;
mod providers;
mod research;

use config::Config;
use monaka_core::manifest::{CONTROLLER_PAD, CONTROLLER_PROXY, MENU_REACH_DEPTH, Manifest, OWN_EYE_SIZE, Opt, RENDER_SIZE, dpad_hold, menu_reach};
use monaka_core::options::Options;
use monaka_hook::module::{self, Module};
use monaka_hook::mem;
use monaka_producer::game::{Adapter, Component, Game, On, Run, Start, Switch};
use monaka_producer::{Rejection, Session, log, require_build};
use providers::engine;
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
            &EYE_SIZE,
            &PRESENT,
            &EYES,
            &UPSCALE,
            &AFW,
            &DRAWS,
            &HUD,
            &HUD_PANELS,
            &extras::ui_size::UI_SIZE,
            &LOOK,
            &PITCH_LOCK,
            &HAND_RIG,
            &FINGERS,
            &MELEE,
            &THROW_BY_HAND,
            &BOW_BY_HAND,
            &extras::grapple::GRAPPLE,
            &LOCKPICK,
            &GUN,
            &ROOM_SCALE,
            &extras::vr_buttons::CONTROLS,
            &CROUCH_BY_DUCKING,
            &RELOAD_BY_HAND,
            &extras::grabs::GRABS,
            &ACTIONS,
            &GAMEPAD,
            &PROBES,
        ],
    };
    type Build = Facts;
    type Frame = ();

    fn prepare(session: &'static Session, manifest: &'static Manifest, options: &Options) -> Result<Facts, Rejection> {
        let engine_module = require_build(eng_chr::ENGINE, engine::ENGINE_SHA256)?;
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

// ---- Stereo ----

/// The game renders at the headset's eye size while VR runs (`monaka_stereo::eye_size`,
/// [`providers::video`]), switched at its install, waiting for it, before anything takes the back
/// buffer's size; with FSR, smaller, and the upscaler makes the eye size. Nothing waits at the
/// presents.
pub const EYE_SIZE: Component<Dl1> = Component {
    options: &[RENDER_SIZE, OWN_EYE_SIZE],
    on: |run| On::when(!research::options().flat && run.options.size("eye_size").is_some(), "the game renders at its own size"),
    install: providers::video::install_eye_size,
    ..monaka_stereo::eye_size::eye_size()
};

/// Each frame to the viewer by Monaka's present loop, with DL1's steps and its eye site
/// (`Presents for Dl1`, [`providers::present_entry`]).
pub const PRESENT: Component<Dl1> = Component { on: stereo_on, ..monaka_stereo::feature::present() };

/// Stereo by alternate eyes, in place ([`providers::eye_site`], installed with the present loop):
/// the view setup moves the cached render camera to the eye the present loop chose, the game asked
/// for a field of view wide enough for both eyes; with pointing aim, the controllers' block is read
/// at each present. No `stereo` switch: the present loop's steps are the eyes'.
pub const EYES: Component<Dl1> = Component {
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

/// FSR upscaling (`monaka_stereo::upscaler::fsr11`): each eye rendered smaller, AMD's FSR bringing
/// it to the eye size, with AMD's DLLs from `fsr_dlls=<folder>` or beside this DLL's copy
/// ([`providers::fsr`]).
pub const UPSCALE: Component<Dl1> = Component {
    options: &[config::FSR, config::FSR_SCALE, config::FSR_SHARPNESS],
    needs: &[&EYES],
    on: |run| On::when(Config::from_options(run.options, run.manifest).fsr.is_some(), "FSR is off"),
    install: providers::fsr::install,
    ..monaka_stereo::upscaler::fsr11::per_eye()
};

/// AFW (Frame-warp, `stereo=afw`): alternate-eye rendering with depth, a pair every frame, the other
/// eye reprojected from the real one (`monaka_stereo::afw11`, on [`providers::depth`]); the
/// HUD a layer over both eyes.
pub const AFW: Component<Dl1> = Component {
    needs: &[&EYES],
    on: |run| monaka_stereo::afw::on(monaka_core::rendering::StereoMethod::chosen(&config::STEREO, run.options)),
    ..monaka_stereo::afw11::frame_warping()
};

// ---- The HUD ----

/// The back buffer's draws tracked on the immediate context ([`providers::hud_draws`]): for the
/// HUD, the upscaler's and AFW's scene copy, and the probes.
pub const DRAWS: Component<Dl1> = Component {
    name: "draws",
    needs: &[&PRESENT],
    on: |run| {
        let config = Config::from_options(run.options, run.manifest);
        On::when(config.hud_active() || config.afw || config.fsr.is_some() || research::options().draws(), "no draws to follow")
    },
    install: providers::hud_draws::install_draws,
    ..Component::NONE
};

/// The HUD and menus in front of the eyes (`hud=0`: as the game draws them): the draws after the
/// world's get a viewport at the HUD's place in each eye ([`providers::hud_draws`], by the frame
/// labels). On the HUD layer's base (`monaka_stereo::hud_layer`), at DL1's own size.
pub const HUD: Component<Dl1> = Component { options: &monaka_stereo::hud_layer::options(0.36), needs: &[&DRAWS], ..monaka_stereo::hud_layer::hud_layer() };

/// The dynamic HUD (Monaka's `monaka_stereo::hud_panels`): the pieces of DL1's HUD on the hands,
/// each HUD draw told apart by the game's UI elements ([`providers::hud_pieces`]).
pub const HUD_PANELS: Component<Dl1> = Component {
    needs: &[&HUD],
    install: providers::hud_pieces::install_panels,
    ..monaka_stereo::hud_panels::hud_panels()
};

/// The tools (left) and weapons (right) stay open while the d-pad is held; picked with A, B, X, Y.
const DPAD_HOLD: Opt = dpad_hold(&["--dpad-hold", "left,right"]);
const MENU_REACH: Opt = menu_reach("Open the wheels by reaching over a shoulder.");

/// The actions provider ([`providers::actions`]): ready-mades press the game's actions by its
/// bindings.
pub const ACTIONS: Component<Dl1> = monaka_pad::feature::actions();

/// The controllers as the game's pad, the whole right stick to the game (head aim owns the facing),
/// with the wheels held open by the d-pad or reached for over a shoulder. `pad_mask_real=1` (a
/// diagnostic) drops a real pad's input.
pub const GAMEPAD: Component<Dl1> = Component {
    options: &[CONTROLLER_PAD, DPAD_HOLD, MENU_REACH, MENU_REACH_DEPTH, CONTROLLER_PROXY],
    needs: &[&PRESENT],
    arm: arm_pad,
    ..monaka_pad::feature::gamepad()
};

fn arm_pad() {
    monaka_pad::feature::whole_stick();
    if providers::eye_site::config().pad_mask_real {
        monaka_pad::mask_real(true);
    }
}

// ---- Look ----

/// Head aim with the mod-owned facing (`monaka_body::look`, its facing delivery on DL1's look fields:
/// [`providers::look_fields`]): the view faces where the player turned it, plus the head; the
/// character is steered there. Reports the cutscenes that take the camera and the climbs.
pub const LOOK: Component<Dl1> = Component { needs: &[&EYES], ..monaka_body::look::look() };

/// Without head aim steering, vertical look passes only back toward level (`monaka_body::pitch_lock`,
/// at the input-action converter: [`providers::look_fields`]).
pub const PITCH_LOCK: Component<Dl1> = Component { needs: &[&EYES], ..monaka_body::pitch_lock::pitch_lock() };

// ---- The arms ----

/// The arms on the controllers at the arms visual's camera-target callback (Monaka's hand rig, on
/// [`providers::skeleton`]), a swing of a held weapon or a fist attacking; what the hands do with
/// the world's objects from where the rig puts them ([`providers::hand_world`]).
pub const HAND_RIG: Component<Dl1> = Component {
    needs: &[&LOOK],
    install: providers::skeleton::install_hand_rig,
    give_back: providers::skeleton::give_back,
    release: || {
        providers::hand_world::report();
        monaka_body::controllers::close();
    },
    ..monaka_body::hand_rig::hand_rig()
};

/// A free hand's fingers on the tracked fingers (the hand rig's, at the arms' pose).
pub const FINGERS: Component<Dl1> = Component { needs: &[&HAND_RIG], ..monaka_body::feature::fingers() };

// ---- What the hands do ----

/// Physical melee (`monaka_body::melee_hit`, on [`providers::melee`]): a swing hits where the
/// weapon in the hand is; with `melee_on_reach` a weapon's swing attacks only once the blade is
/// about to reach something, and with `back_is_blunt` a cutting weapon's hit not led by its edge
/// lands blunt.
pub const MELEE: Component<Dl1> = Component { needs: &[&HAND_RIG], ..monaka_body::melee_hit::physical_melee() };

/// Tools thrown with the left hand's own throw (`monaka_body::throw`, on
/// [`providers::throws`]; `throw_by_hand=0`: the game's throw along the view).
pub const THROW_BY_HAND: Component<Dl1> = Component { needs: &[&HAND_RIG], ..monaka_body::throw::throw() };

/// The bow drawn by pulling the string hand back, loosed along the bow (`monaka_body::bow`, on
/// [`providers::bow`] and [`providers::projectile`]; `bow_by_hand=0`: the game's shot along the
/// view).
pub const BOW_BY_HAND: Component<Dl1> = Component { needs: &[&HAND_RIG], ..monaka_body::bow::bow() };

/// A gun's shots leave its muzzle along its barrel (`monaka_body::gun`, on
/// [`providers::projectile`] and the gun's line in the hand, [`providers::skeleton`];
/// `shoot_from_barrel=0`: from the eye along the aim).
pub const GUN: Component<Dl1> = Component { needs: &[&HAND_RIG], ..monaka_body::gun::gun() };

/// Lockpicking by twisting the hands (`monaka_body::lockpick`, on [`providers::lockpick`];
/// `lockpick_by_hand=0`: the sticks).
pub const LOCKPICK: Component<Dl1> = Component { needs: &[&HAND_RIG], ..monaka_body::lockpick::lockpick() };

/// The character follows the player's real steps (`monaka_body::feature::room_scale` at DL1's own
/// walk body, [`providers::steps`], each step measured at the next one), with the hand rig.
pub const ROOM_SCALE: Component<Dl1> = Component { needs: &[&HAND_RIG], ..monaka_body::feature::room_scale() };

/// Crouching by ducking (`monaka_body::duck`), held on the VR button layout's hold-to-crouch
/// ([`extras::vr_buttons`]): the eye site hands it the head's height.
pub const CROUCH_BY_DUCKING: Component<Dl1> = Component { needs: &[&extras::vr_buttons::CONTROLS], ..monaka_body::duck::duck() };

/// Reloading by hand (`monaka_body::reload`), with the hand rig (`reload_gesture=0` turns it off):
/// read and pressed on the VR button layout's input pass ([`extras::vr_buttons`]).
pub const RELOAD_BY_HAND: Component<Dl1> = Component {
    needs: &[&HAND_RIG, &extras::vr_buttons::CONTROLS],
    switch: Some(Switch::new("reload_gesture", true, "reloading by hand is off")),
    ..monaka_body::reload::reload()
};

/// The research probes: their hooks, each checking its own option; the look and arms probes take
/// their component's hook when it is off.
pub const PROBES: Component<Dl1> = Component { install: research::install_probes, quiet: research::report, ..monaka_producer::game::probes() };
