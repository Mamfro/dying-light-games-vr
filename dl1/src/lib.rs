//! Dying Light 1 in head-tracked stereo: alternate-eye rendering in place, published to the
//! viewer through the D3D11 keyed-mutex channel. Injected by `monaka_loader`; options come from
//! `dl1-options.txt` (`config`) and the channel from `live-channel.txt` beside the DLL copy.
//!
//! Game requirements: NVIDIA HBAO+, depth of field and PCSS off (they get the wrong camera and
//! make shadows slide), and no `/3dtv` launch option.
//!
//! The adapter ([`Dl1::ADAPTER`]), in the order its components start:
//!
//! - [`EYE_SIZE`]: the game switched to the headset's eye size first (the channel, the HUD and the
//!   upscaler take the back buffer's size); [`PRESENT`]: each frame to the viewer by Monaka's
//!   present loop, with DL1's own steps around it (`view::stereo`);
//! - [`EYES`]: stereo by alternate eyes, the cached render camera moved to each eye at the view
//!   setup; [`UPSCALE`]: FSR; [`HYBRID`]: the depth experiment;
//! - [`DRAWS`]: the back buffer's draws tracked; [`HUD`]: the HUD placed in front of the eyes;
//!   [`PANELS`]: its pieces on the hands; [`UI_SIZE`]: the game's UI kept at the monitor's size;
//! - [`LOOK`]: head aim with the mod-owned facing; [`ARMS`]: the arms on the controllers;
//!   [`FINGERS`]: their fingers; [`MELEE`], [`THROW`], [`BOW`], [`GRAPPLE`], [`LOCKPICK`],
//!   [`GUN`], [`ROOM_SCALE`]: what the hands and steps do;
//! - [`CONTROLS`]: the VR button layout and crouching; [`GRABS`]; [`GAMEPAD`];
//! - research: [`PROBES`].
//!
//! `flat=1` (research) runs the probes alone on the game as it plays on the monitor: everything
//! that needs stereo is off; `flat_buttons=1` keeps the buttons and grabs.

mod config;
mod engine;
mod hud;
mod manifest;
#[cfg(test)]
mod launch_tests;
mod output;
mod player;
mod research;
mod view;

use config::Config;
use engine::{CameraWriter, SetInverseFn};
use eng_chr::feature::DISABLE_ZOMBIE_GRABS;
use hud::{draws, panels, ui};
use monaka_channel::hands::HandChannel;
use monaka_core::feature::Feature;
use monaka_core::manifest::{
    AIM_HAND, AIM_LOOK, AIM_MOTION, AIM_MOUSE, AIM_POINT, CONTROLLER_PAD, CONTROLLER_PROXY, EYE_SEPARATION, HUD_DISTANCE, HUD_SIZE, Kind, MENU_REACH_DEPTH, Manifest,
    Opt, Shown, SteamFiles, aim, aims, choice, dpad_hold, menu_reach, opt, render_size,
};
use monaka_core::options::Options;
use monaka_core::situation::Situation;
use monaka_hook::module::{self, Module};
use monaka_hook::{Hooks, mem, slot};
use monaka_producer::game::{Adapter, Component, Game, Lack, On, Run, Start};
use monaka_producer::{Rejection, Session, log, require_build};
use monaka_stereo::feature::{Graphics, Loop, Presents};
use output::{hybrid, upscale, video};
use player::{aim, bow, controls, grabs, gun, hand_world, hands, lockpick, melee, spots, throwing, walk};
use std::ffi::c_void;
use view::stereo;
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
            &HYBRID,
            &DRAWS,
            &HUD,
            &PANELS,
            &UI_SIZE,
            &LOOK,
            &ARMS,
            &FINGERS,
            &MELEE,
            &THROW,
            &BOW,
            &GRAPPLE,
            &LOCKPICK,
            &GUN,
            &ROOM_SCALE,
            &CONTROLS,
            &GRABS,
            &GAMEPAD,
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

/// DL1's own steps around Monaka's present loop (`view::stereo`): the upscaler, the hybrid and a
/// flat screen publish their own pairs; the HUD's draws end each frame; the end gives back what the
/// run changed.
impl Presents for Dl1 {
    const LOOP: Loop<Self> = Loop {
        frame_end: Some(draws::frame_end),
        report: Some(stereo::report),
        before: Some(stereo::before_present),
        after: Some(stereo::after_present),
        finish: Some(stereo::finish),
        ..Loop::new(|_| Graphics::D3D11, ("camera shifts", &stereo::SHIFTS))
    };
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

// ---- Stereo ----

/// The game renders at the headset's eye size while VR runs (`output::video`), switched before
/// anything takes the back buffer's size; with FSR, smaller, and the upscaler makes the eye size.
pub const EYE_SIZE: Component<Dl1> = Component {
    name: "eye size",
    options: &[render_size("If the game crashes, reset its resolution.")],
    on: |run| On::when(!research::options().flat && run.options.size("eye_size").is_some(), "the game renders at its own size"),
    install: |start| {
        let config = config(start);
        let Some(size) = config.eye_size else { return Ok(()) };
        let render = config.upscaled().map_or(size, |fsr| fsr.render_size(size));
        if let Err(why) = video::switch_to(&start.build.gamedll, &game_swapchain(&start.build.renderer)?, render, std::time::Duration::from_secs(8)) {
            log!("rendering at the game's own size: {why}");
        }
        Ok(())
    },
    // The game applies it on its next frame; its levels then lay their menus out again.
    give_back: video::restore_and_relayout,
    ..Component::NONE
};

/// Each frame to the viewer by Monaka's present loop, with DL1's steps ([`Dl1::LOOP`]).
pub const PRESENT: Component<Dl1> = Component { on: stereo_on, ..monaka_stereo::feature::present() };

/// Stereo by alternate eyes, in place (`view::stereo`): the view setup moves the cached render
/// camera to the eye the present loop chose, the game asked for a field of view wide enough for
/// both eyes; with pointing aim, the controllers' block is read at each present.
pub const EYES: Component<Dl1> = Component {
    name: "eyes",
    features: &[Feature::Stereo],
    reports: &[Situation::WorldHidden],
    options: &[EYE_SEPARATION],
    needs: &[&PRESENT],
    needs_channel: true,
    settings: |options, manifest| {
        Config::from_options(options, manifest);
    },
    install: install_eyes,
    release: stereo::release,
    ..Component::NONE
};

fn install_eyes(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = config(start);
    let engine_module = &start.build.engine;
    let set_inverse = engine_module.export(engine::SET_INVERSE_CAMERA).ok_or_else(|| Rejection::revision("the engine does not export SetInvCameraMatrix"))?;
    if let Some(missing) = engine::MESH_CULL_SETTERS.iter().find(|name| engine_module.export(name).is_none()) {
        return Err(Rejection::revision(format!("the engine does not export {missing}")).into());
    }
    let channel = start.channel()?;
    // Hand aim reads the controllers the viewer writes beside the pose; without them, head aim.
    let hands = if config.aim.hand { HandChannel::open(channel).map_err(|e| log!("hand aim off: hand block unavailable: {e}")).ok() } else { None };
    // SAFETY: the export is `void IBaseCamera::SetInvCameraMatrix(const mtx34&)` on x64.
    let camera = CameraWriter(unsafe { std::mem::transmute::<usize, SetInverseFn>(set_inverse) });
    stereo::install(stereo::Setup { config, camera }, hands);
    // SAFETY: each target is checked against its exact prologue; each detour has its signature.
    unsafe {
        start.hooks.inline(&stereo::VIEW_SETUP_ORIGINAL, "view setup", engine_module.at(engine::VIEW_SETUP), &engine::VIEW_SETUP_PROLOGUE, stereo::view_setup)?;
        // The field-of-view widening serves both the live-camera turn and head aim.
        if config.headset_fov && config.widen_game_fov {
            let set_fov = engine_module.export(engine::SET_FOV).ok_or_else(|| Rejection::revision("the engine does not export SetFOV"))?;
            start.hooks.inline(&stereo::SET_FOV_ORIGINAL, "SetFOV", set_fov, &engine::SET_FOV_PROLOGUE, stereo::set_fov)?;
        }
    }
    Ok(())
}

const FSR: Opt = opt("fsr", "FSR upscaling", "Renders each eye smaller, then upscales it (needs the headset resolution and Dying Light 2's FidelityFX DLLs).", Kind::Switch { default: false })
    .on(Shown::Engine)
    // Upscales to the eye size, with the FidelityFX runtime Dying Light 2 installs (Monaka ships none).
    .needs_eye_size()
    .needs_steam_files(SteamFiles {
        game_folder: "Dying Light 2",
        path: r"ph\work\bin\x64",
        file: "amd_fidelityfx_upscaler_dx12.dll",
        line: "fsr_dlls",
        missing: "FSR is off: it needs AMD's FidelityFX DLLs, which come with Dying Light 2 (not installed).",
    });
const FSR_SCALE: Opt = opt("fsr_scale", "FSR quality", "How much smaller each eye renders.", choice(&[("1.5", "Quality"), ("1.7", "Balanced"), ("2", "Performance"), ("3", "Ultra performance")], "1.5"))
    .on(Shown::Engine)
    .only_when("fsr", "1");
const FSR_SHARPNESS: Opt =
    opt("fsr_sharpness", "FSR sharpness", "0 turns sharpening off.", Kind::Number { min: 0.0, max: 1.0, step: 0.05, default: 0.3 }).on(Shown::Engine).only_when("fsr", "1");

/// FSR upscaling (`output::upscale`): each eye rendered smaller, AMD's FSR bringing it to the eye
/// size, with AMD's DLLs from `fsr_dlls=<folder>` or beside this DLL's copy.
pub const UPSCALE: Component<Dl1> = Component {
    name: "fsr",
    features: &[Feature::PerEyeHistory],
    options: &[FSR, FSR_SCALE, FSR_SHARPNESS],
    needs: &[&EYES],
    on: |run| {
        let config = Config::from_options(run.options, run.manifest);
        On::when(config.fsr.is_some(), "FSR is off")
    },
    install: install_upscale,
    ..Component::NONE
};

fn install_upscale(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = config(start);
    let (Some(fsr), Some(display)) = (config.upscaled(), config.eye_size) else {
        return Err(Lack::Missing("FSR needs the headset's eye size and does not combine with hybrid".to_owned()));
    };
    let dlls = start.options.text("fsr_dlls").map_or_else(|| start.session.dir().to_path_buf(), std::path::PathBuf::from);
    let (_, device, _, render) = swapchain_parts(&start.build.renderer)?;
    log!("{}", upscale::install(&device, fsr, render, display, &dlls).map_err(Lack::Missing)?);
    Ok(())
}

/// Experiment (`hybrid=1`): alternate-eye rendering with depth, a pair every frame, the other eye
/// reprojected from the real one (`output::hybrid`); the HUD a layer over both eyes.
pub const HYBRID: Component<Dl1> = Component {
    name: "hybrid",
    needs: &[&EYES],
    on: |run| On::when(run.options.switch("hybrid", false), "alternate eyes without depth"),
    install: |start| {
        let hud = start.shared.hud.shown;
        let (_, device, _, _) = swapchain_parts(&start.build.renderer)?;
        hybrid::install(&device, start.channel()?.clone(), hud).map_err(Lack::Missing)?;
        log!("alternate-eye with depth: a pair every frame; HUD layer {hud}");
        Ok(())
    },
    ..Component::NONE
};

// ---- The back buffer's draws and the HUD ----

/// The back buffer's draws tracked on the immediate context (`hud::draws`): for the HUD, the
/// upscaler's and the hybrid's scene copy, and the probes.
pub const DRAWS: Component<Dl1> = Component {
    name: "draws",
    needs: &[&PRESENT],
    on: |run| {
        let config = Config::from_options(run.options, run.manifest);
        On::when(config.hud_active() || config.hybrid || config.fsr.is_some() || research::options().draws(), "no draws to follow")
    },
    install: install_draws,
    ..Component::NONE
};

fn install_draws(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = config(start);
    let (_, _, context, (width, height)) = swapchain_parts(&start.build.renderer)?;
    if config.hud_active() || config.upscaled().is_some() {
        draws::install(draws::HudSettings { distance: config.hud.distance, scale: config.hud.scale, backbuffer_width: width, backbuffer_height: height });
    }
    draws::enable();
    let context = context.as_raw();
    let hooks = &mut *start.hooks;
    // SAFETY: method slots of the game's live immediate context; each detour has its signature.
    unsafe {
        hook_method(hooks, &draws::TARGETS, "OMSetRenderTargets", context, slot::CONTEXT_OM_SET_RENDER_TARGETS, draws::set_targets)?;
        hook_method(hooks, &draws::DRAW_INDEXED, "DrawIndexed", context, slot::CONTEXT_DRAW_INDEXED, draws::draw_indexed)?;
        hook_method(hooks, &draws::DRAW, "Draw", context, slot::CONTEXT_DRAW, draws::draw)?;
        hook_method(hooks, &draws::DRAW_INDEXED_INSTANCED, "DrawIndexedInstanced", context, slot::CONTEXT_DRAW_INDEXED_INSTANCED, draws::draw_indexed_instanced)?;
        hook_method(hooks, &draws::DRAW_INSTANCED, "DrawInstanced", context, slot::CONTEXT_DRAW_INSTANCED, draws::draw_instanced)?;
    }
    Ok(())
}

/// The HUD and menus in front of the eyes (`hud=0`: as the game draws them): the draws after the
/// world's get a viewport at the HUD's place in each eye (`hud::draws`).
pub const HUD: Component<Dl1> = Component {
    name: "hud",
    features: &[Feature::HudLayer],
    options: &[HUD_SIZE.default_number(0.36), HUD_DISTANCE],
    needs: &[&DRAWS],
    on: |run| On::when(run.shared.hud.shown, "the HUD stays as the game draws it"),
    ..Component::NONE
};

/// The dynamic HUD (`hud::panels`): each HUD piece's draws, told apart by the game's UI elements
/// (`hud::ui`) and placed by the matrix the game maps for it, to the panels on the hands.
pub const PANELS: Component<Dl1> = Component {
    name: "panels",
    features: &[Feature::HudPanels],
    options: &[monaka_core::manifest::WORLD_HUD],
    needs: &[&HUD],
    on: |run| On::when(run.shared.hud.world && !run.options.switch("hybrid", false), "the dynamic HUD is off"),
    install: install_panels,
    ..Component::NONE
};

fn install_panels(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    panels::install(start.channel()?);
    let (_, _, context, _) = swapchain_parts(&start.build.renderer)?;
    let context = context.as_raw();
    // SAFETY: method slots of the game's live immediate context; each detour has its signature.
    unsafe {
        hook_method(start.hooks, &panels::MAP, "Map", context, panels::CONTEXT_MAP, panels::map)?;
        hook_method(start.hooks, &panels::UNMAP, "Unmap", context, panels::CONTEXT_UNMAP, panels::unmap)?;
    }
    ui::install(start.hooks, &start.build.engine)?;
    Ok(())
}

/// The game's UI kept at the monitor's size while it renders at the eye size (`hud::ui_size`; it
/// passes through when no size switch was made).
pub const UI_SIZE: Component<Dl1> = Component {
    name: "ui size",
    needs: &[&PRESENT],
    install: |start| {
        hud::ui_size::install(start.hooks, &start.build.engine, &start.build.gamedll)?;
        Ok(())
    },
    ..Component::NONE
};

// ---- Look and the hand rig ----

/// Head aim with the mod-owned facing (`player::aim`): the character turned by the head at
/// `FromForwardUpPos`, the look input actions held; spots to come back to are taken at its updates
/// (`player::spots`). Reports the cutscenes that take the camera and the climbs.
pub const LOOK: Component<Dl1> = Component {
    name: "look",
    features: &[Feature::Look],
    reports: &[Situation::CameraTaken, Situation::Traversal],
    options: &[aim(&[AIM_MOTION, AIM_LOOK, AIM_POINT, AIM_MOUSE], aims::HAND_RIG), AIM_HAND],
    needs: &[&EYES],
    on: |run| On::when(run.shared.aim.head, "aim is mouse and stick only"),
    install: install_look,
    ..Component::NONE
};

fn install_look(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let Facts { engine: engine_module, gamedll, .. } = start.build;
    spots::install(engine_module, start.session.dir())?;
    hook_from_forward(start.hooks, engine_module)?;
    aim::look_actions_match(gamedll).map_err(Rejection::revision)?;
    aim::resolve(engine_module)?;
    // SAFETY: the converter's exact prologue is checked; the detour has its signature.
    unsafe { start.hooks.inline(&aim::INPUT_ACTION_ORIGINAL, "input actions", gamedll.at(engine::INPUT_ACTION), &engine::INPUT_ACTION_PROLOGUE, aim::input_action)? };
    Ok(())
}

/// The player camera's setter, where head aim steers (and its probe watches).
fn hook_from_forward(hooks: &mut Hooks, engine_module: &Module) -> Result<(), Lack> {
    let target = engine_module.export(engine::FROM_FORWARD).ok_or_else(|| Rejection::revision("the engine does not export FromForwardUpPos"))?;
    // SAFETY: the export, checked against its exact prologue; the detour has its signature.
    unsafe { hooks.inline(&aim::FROM_FORWARD_ORIGINAL, "FromForwardUpPos", target, &engine::FROM_FORWARD_PROLOGUE, aim::from_forward)? };
    Ok(())
}

/// The arms on the controllers at the arms visual's camera-target callback (`player::hands`), a
/// swing of a held weapon or a fist attacking; what the hands do with the world's objects from
/// where the rig puts them (`player::hand_world`).
pub const ARMS: Component<Dl1> = Component {
    name: "arms",
    features: &[Feature::HandRig],
    needs: &[&LOOK],
    on: |run| On::when(run.shared.aim.rig, "aim is not motion controls"),
    install: |start| {
        hook_arms(start.hooks, &start.build.engine, &start.build.gamedll)?;
        hand_world::resolve(&start.build.engine)?;
        Ok(())
    },
    release: hand_world::report,
    ..Component::NONE
};

/// The arms visual's camera-target callback (the rig, and its probes).
fn hook_arms(hooks: &mut Hooks, engine_module: &Module, gamedll: &Module) -> Result<(), Lack> {
    hands::resolve(engine_module)?;
    // SAFETY: the callback's exact prologue is checked; the detour has its signature.
    unsafe { hooks.inline(&hands::CAMERA_TARGET_ORIGINAL, "arms camera target", gamedll.at(engine::FPP_CAMERA_TARGET), &engine::FPP_CAMERA_TARGET_PROLOGUE, hands::camera_target)? };
    Ok(())
}

/// A free hand's fingers on the tracked fingers (`eng_chr::fingers` at the arms' pose), read from
/// the controllers' finger block.
pub const FINGERS: Component<Dl1> = Component {
    needs: &[&ARMS],
    needs_channel: true,
    install: |start| {
        monaka_arms::controllers::open(start.channel()?);
        monaka_arms::fingers::set_binary(start.shared.fingers.binary);
        Ok(())
    },
    release: monaka_arms::controllers::close,
    ..monaka_arms::feature::fingers()
};

/// Physical melee (`player::melee`): a swing hits where the weapon in the hand is; with
/// `melee_on_reach` a weapon's swing attacks only once the blade is about to reach something, and
/// with `back_is_blunt` a cutting weapon's hit not led by its edge lands blunt.
pub const MELEE: Component<Dl1> = Component {
    name: "melee",
    features: &[Feature::Melee],
    needs: &[&ARMS],
    on: |run| On::when(Config::from_options(run.options, run.manifest).physical_melee.is_some(), "physical melee is off"),
    install: |start| {
        let config = config(start);
        let Some(physical) = config.physical_melee else { return Ok(()) };
        melee::install(start.hooks, &start.build.gamedll, &start.build.engine, physical, config.melee_on_reach, config.back_is_blunt)?;
        Ok(())
    },
    ..Component::NONE
};

/// Tools thrown with the left hand's own throw (`player::throwing`; `throw_by_hand=0`: the game's
/// throw along the view).
pub const THROW: Component<Dl1> = Component {
    name: "throw",
    features: &[Feature::Throw],
    needs: &[&ARMS],
    on: |run| On::when(run.options.switch("throw_by_hand", true), "the game's throw"),
    install: |start| Ok(throwing::install(start.hooks, &start.build.gamedll, &start.build.engine)?),
    release: throwing::report,
    ..Component::NONE
};

/// The bow drawn by pulling the string hand back, loosed along the hands (`player::bow`;
/// `bow_by_hand=0`: the game's shot along the view).
pub const BOW: Component<Dl1> = Component {
    name: "bow",
    features: &[Feature::Bow],
    needs: &[&ARMS],
    on: |run| On::when(run.options.switch("bow_by_hand", true), "the game's bow"),
    install: |start| Ok(bow::install(start.hooks, &start.build.gamedll)?),
    release: bow::report,
    ..Component::NONE
};

/// The grappling hook goes where the left controller points (`player::grapple`;
/// `grapple_by_hand=0`: where the character looks).
pub const GRAPPLE: Component<Dl1> = Component {
    name: "grapple",
    needs: &[&ARMS],
    on: |run| On::when(run.options.switch("grapple_by_hand", true), "the game's grapple"),
    install: |start| Ok(player::grapple::install(start.hooks, &start.build.gamedll)?),
    release: player::grapple::report,
    ..Component::NONE
};

/// Lockpicking by twisting the hands (`player::lockpick`; `lockpick_by_hand=0`: the sticks).
pub const LOCKPICK: Component<Dl1> = Component {
    name: "lockpick",
    features: &[Feature::Lockpick],
    needs: &[&ARMS],
    on: |run| On::when(run.options.switch("lockpick_by_hand", true), "the game's lockpick"),
    install: |start| Ok(lockpick::install(start.hooks, &start.build.gamedll)?),
    release: lockpick::report,
    ..Component::NONE
};

/// A gun's shots leave its muzzle along its barrel (`player::gun`; `shoot_from_barrel=0`: from the
/// eye along the aim).
pub const GUN: Component<Dl1> = Component {
    name: "gun",
    needs: &[&ARMS],
    on: |run| On::when(run.options.switch("shoot_from_barrel", true), "shots from the eye"),
    install: |start| Ok(gun::install(start.hooks, &start.build.gamedll)?),
    release: gun::report,
    ..Component::NONE
};

/// The character follows the player's real steps (`monaka_arms::roomscale` at DL1's own walk body,
/// `player::walk`), with the hand rig; the walk body is put back as the game had it.
pub const ROOM_SCALE: Component<Dl1> = Component {
    name: "room scale",
    features: &[Feature::RoomScale],
    options: &[monaka_core::manifest::ROOM_SCALE],
    needs: &[&ARMS],
    on: |run| On::when(monaka_arms::roomscale::Settings::from_options(run.options, run.shared).is_some(), "room-scale movement is off"),
    install: |start| {
        let Some(settings) = monaka_arms::roomscale::Settings::from_options(start.options, start.shared) else { return Ok(()) };
        walk::install(start.hooks, &start.build.engine)?;
        monaka_arms::roomscale::install(settings);
        Ok(())
    },
    quiet: monaka_arms::roomscale::stop,
    give_back: walk::restore,
    ..Component::NONE
};

// ---- Buttons and pad ----

const VR_BUTTONS: Opt =
    opt("vr_buttons", "VR buttons", "B turns the flashlight on and off, tap Y to heal (hold Y still repairs), and duck in your room to crouch. Off: the game's own buttons.", Kind::Switch { default: true })
        .on(Shown::Game);

/// The VR button layout (`player::controls`): B the flashlight, tap Y to heal, crouching by
/// ducking; with the hand rig, the left hand to the gun and away reloads (`reload_gesture`).
pub const CONTROLS: Component<Dl1> = Component {
    name: "controls",
    features: &[Feature::Duck, Feature::Reload],
    options: &[VR_BUTTONS],
    settings: |options, _| {
        options.switch("vr_buttons", true);
    },
    on: |run| On::when(run.options.switch("vr_buttons", true) && flat_kept(), "the game's own buttons"),
    install: |start| {
        let config = config(start);
        let reload = config.reload_gesture && config.aim.rig && start.is_on(&ARMS);
        controls::install(start.hooks, &start.build.gamedll, &start.build.engine, reload)?;
        Ok(())
    },
    quiet: controls::stop,
    ..Component::NONE
};

/// Zombies' grabs never land on the player (`player::grabs`: the player's own may-be-grabbed check).
pub const GRABS: Component<Dl1> = Component {
    name: "grabs",
    options: &[DISABLE_ZOMBIE_GRABS],
    settings: |options, _| {
        options.switch("disable_zombie_grabs", true);
    },
    on: |run| On::when(run.options.switch("disable_zombie_grabs", true) && flat_kept(), "zombies grab as in the game"),
    install: |start| Ok(grabs::install(start.hooks, &start.build.gamedll)?),
    release: grabs::report,
    ..Component::NONE
};

/// The tools (left) and weapons (right) stay open while the d-pad is held; picked with A, B, X, Y.
const DPAD_HOLD: Opt = dpad_hold(&["--dpad-hold", "left,right"]);
const MENU_REACH: Opt = menu_reach(
    "Rest your left hand over your left shoulder for the tools, your right hand over your right shoulder for the weapons. Flick a stick toward the one you want; bring the hand back to close.",
);

/// The controllers as the game's pad, the whole right stick to the game (head aim owns the facing).
/// `pad_mask_real=1` (a diagnostic) drops a real pad's input.
pub const GAMEPAD: Component<Dl1> = Component {
    options: &[CONTROLLER_PAD, DPAD_HOLD, MENU_REACH, MENU_REACH_DEPTH, CONTROLLER_PROXY],
    needs: &[&PRESENT],
    arm: || {
        monaka_pad::feature::whole_stick();
        if stereo::config().pad_mask_real {
            monaka_pad::mask_real(true);
        }
    },
    ..monaka_pad::feature::gamepad()
};

// ---- Research ----

/// The research probes: their hooks, each checking its own option; the look and arms probes take
/// their component's hook when it is off.
pub const PROBES: Component<Dl1> = Component { install: install_probes, quiet: research::report, ..monaka_producer::game::probes() };

fn install_probes(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let probes = research::options();
    let Facts { engine: engine_module, renderer, gamedll } = start.build;
    if !probes.flat {
        if probes.look && !start.is_on(&LOOK) {
            hook_from_forward(start.hooks, engine_module)?;
        }
        if probes.arms() && !start.is_on(&ARMS) {
            hook_arms(start.hooks, engine_module, gamedll)?;
        }
    }
    let size = if probes.flat { None } else { swapchain_parts(renderer).ok().map(|(_, _, _, size)| size) };
    // SAFETY: the engine and the game DLL are the builds `engine.rs` describes (hashes checked).
    unsafe { research::install(start.hooks, engine_module, gamedll, size)? };
    Ok(())
}
