//! Dying Light 1 in head-tracked stereo: alternate-eye rendering in place, published to the
//! viewer through the D3D11 keyed-mutex channel. Injected by `monaka_loader`; options come from
//! `dl1-options.txt` (`config`, the manifest's options) and the channel from `live-channel.txt`
//! beside the DLL copy.
//!
//! Game requirements: NVIDIA HBAO+, depth of field and PCSS off (they get the wrong camera and
//! make shadows slide), and no `/3dtv` launch option.

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

use view::stereo;
use output::{hybrid, upscale, video};
use hud::{draws, panels, ui};
use player::{aim, bow, controls, grabs, gun, hand_world, hands, lockpick, melee, spots, throwing, walk};

use config::Config;
use monaka_channel::hands::HandChannel;
use monaka_channel::pose::PoseChannel;
use monaka_hook::module::{self, Module};
use monaka_hook::{Hooks, mem, slot};
use monaka_producer::{Rejection, Session, enable_hooks, log, remove_hooks, require_build};
use engine::{CameraWriter, SetInverseFn};
use std::ffi::c_void;
use std::sync::Mutex;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;
use windows::core::{IUnknown, Interface};

monaka_producer::producer!(manifests: crate::manifest::MANIFESTS, start: start, stop: stop);

static HOOKS: Mutex<Option<Hooks>> = Mutex::new(None);

/// [`Hooks::method`] (in the code, the vtable slot as a fallback), logging a fallback.
///
/// # Safety
/// `object` must be a live COM object whose method `index` has type `F`.
unsafe fn hook_method<F: Copy>(
    hooks: &mut Hooks,
    original: &monaka_hook::Original<F>,
    name: &str,
    object: *mut c_void,
    index: usize,
    detour: F,
) -> Result<(), Rejection> {
    // SAFETY: the caller's guarantee.
    Ok(unsafe { hooks.method_noted(original, name, object, index, detour, |why| log!("{why}; hooking its vtable slot instead")) }?)
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

fn start(session: &Session) -> Result<(), Rejection> {
    let engine_module = require_build(engine::ENGINE, engine::ENGINE_SHA256)?;
    let renderer = require_build(engine::RENDERER, engine::RENDERER_SHA256)?;
    let options = session.manifest_options(&manifest::MANIFESTS[0]);
    let config = Config::from_options(&options);
    log!("{config:?}");
    crate::research::configure(&options, session.dir());
    let flat_buttons = |hooks: &mut monaka_hook::Hooks| {
        let gamedll = require_build(engine::GAMEDLL, engine::GAMEDLL_SHA256)?;
        controls::install(hooks, &gamedll, &engine_module, false)?;
        grabs::install(hooks, &gamedll)
    };
    if let Some(started) = crate::research::flat(&engine_module, flat_buttons) {
        return started;
    }
    if config.fsr.is_some() && (config.hybrid || config.eye_size.is_none()) {
        return Err(Rejection::unsupported("fsr: needs eye_size (the headset's) and does not combine with hybrid"));
    }
    if config.aim.head {
        // Spots to come back to (`tools\Spot-DL1.ps1`), taken at head aim's updates.
        spots::install(&engine_module, session.dir())?;
    }
    // AMD's DLLs: `fsr_dlls=<folder>`, or beside this DLL's copy.
    let fsr_dlls = options.text("fsr_dlls").map_or_else(|| session.dir().to_path_buf(), std::path::PathBuf::from);
    let channel = session.require_channel()?;
    monaka_producer::install_crash_log();
    let set_inverse = engine_module
        .export(engine::SET_INVERSE_CAMERA)
        .ok_or_else(|| Rejection::revision("the engine does not export SetInvCameraMatrix"))?;
    if let Some(missing) = engine::MESH_CULL_SETTERS.iter().find(|name| engine_module.export(name).is_none()) {
        return Err(Rejection::revision(format!("the engine does not export {missing}")));
    }
    let poses = if config.head_tracking {
        Some(PoseChannel::open(&channel).map_err(|e| Rejection::not_ready(format!("pose channel: {e}")))?)
    } else {
        None
    };

    // The headset's eye size first: the channel, HUD and hybrid all take the back buffer's size.
    // With FSR the game renders smaller and the upscaler makes the eye size.
    if let Some(size) = config.eye_size {
        let gamedll = require_build(engine::GAMEDLL, engine::GAMEDLL_SHA256)?;
        let render = config.fsr.map_or(size, |fsr| fsr.render_size(size));
        if let Err(why) = video::switch_to(&gamedll, &game_swapchain(&renderer)?, render, std::time::Duration::from_secs(8)) {
            log!("rendering at the game's own size: {why}");
        }
    }
    if config.hud.world && config.hud_active() && !config.hybrid {
        panels::install(&channel);
    }
    let started = attach(config, engine_module, renderer, channel, poses, set_inverse, &fsr_dlls);
    if started.is_err() {
        video::restore();
    }
    started
}

/// The rest of [`start`], at the size the game now renders at.
fn attach(
    config: Config,
    engine_module: Module,
    renderer: Module,
    channel: monaka_channel::ChannelName,
    poses: Option<PoseChannel>,
    set_inverse: usize,
    fsr_dlls: &std::path::Path,
) -> Result<(), Rejection> {
    let swap = game_swapchain(&renderer)?;
    // SAFETY: COM calls on the game's live swapchain and its device.
    let (context, desc) = unsafe {
        let device: ID3D11Device = swap.GetDevice().map_err(|e| Rejection::not_ready(format!("swapchain device: {e}")))?;
        let context = device.GetImmediateContext().map_err(|e| Rejection::not_ready(format!("immediate context: {e}")))?;
        let desc = swap.GetDesc().map_err(|e| Rejection::not_ready(format!("swapchain description: {e}")))?;
        (context, desc)
    };
    log!("swapchain {}x{} format {}", desc.BufferDesc.Width, desc.BufferDesc.Height, desc.BufferDesc.Format.0);

    // SAFETY: the export is `void IBaseCamera::SetInvCameraMatrix(const mtx34&)` on x64.
    let writer = CameraWriter(unsafe { std::mem::transmute::<usize, SetInverseFn>(set_inverse) });
    let channel_for_hybrid = channel.clone();
    let channel_for_pad = channel.clone();
    // The finger block (finger tracking), read through the shared controllers module.
    if config.aim.rig && config.fingers.tracking {
        monaka_arms::controllers::open(&channel);
        monaka_arms::fingers::set_binary(config.fingers.binary);
    }
    // Hand aim reads the controllers the viewer writes beside the pose; without them, head aim.
    let hands = if config.aim.hand {
        HandChannel::open(&channel).map_err(|e| log!("hand aim off: hand block unavailable: {e}")).ok()
    } else {
        None
    };
    stereo::install(stereo::Setup { config, camera: writer, channel }, poses, hands);
    if config.hud_active() || config.fsr.is_some() {
        draws::install(draws::HudSettings {
            distance: config.hud.distance,
            scale: config.hud.scale,
            backbuffer_width: desc.BufferDesc.Width,
            backbuffer_height: desc.BufferDesc.Height,
        });
    }
    if let (Some(fsr), Some(display)) = (config.fsr, config.eye_size) {
        // SAFETY: COM call on the game's live swapchain.
        let device: ID3D11Device = unsafe { swap.GetDevice() }.map_err(|e| Rejection::not_ready(format!("swapchain device: {e}")))?;
        let render = (desc.BufferDesc.Width, desc.BufferDesc.Height);
        log!("{}", upscale::install(&device, fsr, render, display, fsr_dlls).map_err(Rejection::unsupported)?);
    }
    if config.hybrid {
        // SAFETY: COM call on the game's live swapchain.
        let device: ID3D11Device = unsafe { swap.GetDevice() }.map_err(|e| Rejection::not_ready(format!("swapchain device: {e}")))?;
        // The HUD becomes a layer laid over both eyes, so it cannot flicker between them.
        hybrid::install(&device, channel_for_hybrid.clone(), config.hud_active()).map_err(Rejection::unsupported)?;
        log!("alternate-eye with depth: a pair every frame; HUD layer {}", config.hud_active());
    }
    // The back-buffer draw tracking serves the HUD, the probes and the hybrid's capture.
    let draw_hooks = config.hud_active() || config.hybrid || config.fsr.is_some() || crate::research::options().draws();
    if draw_hooks {
        draws::enable();
    }

    let mut hooks = Hooks::default();
    // SAFETY: each target is checked against its exact prologue or is a method slot of the
    // game's live swapchain or immediate context; each detour has the target's signature.
    unsafe {
        let view_setup = engine_module.at(engine::VIEW_SETUP);
        hooks
            .inline(&stereo::VIEW_SETUP_ORIGINAL, "view setup", view_setup, &engine::VIEW_SETUP_PROLOGUE, stereo::view_setup)
            ?;
        // The field-of-view widening serves both the live-camera turn and head aim.
        if config.headset_fov && config.widen_game_fov {
            let set_fov = engine_module.export(engine::SET_FOV).ok_or_else(|| Rejection::revision("the engine does not export SetFOV"))?;
            hooks.inline(&stereo::SET_FOV_ORIGINAL, "SetFOV", set_fov, &engine::SET_FOV_PROLOGUE, stereo::set_fov)?;
        }
        if config.aim.head || crate::research::options().look {
            let target = engine_module.export(engine::FROM_FORWARD).ok_or_else(|| Rejection::revision("the engine does not export FromForwardUpPos"))?;
            hooks
                .inline(&aim::FROM_FORWARD_ORIGINAL, "FromForwardUpPos", target, &engine::FROM_FORWARD_PROLOGUE, aim::from_forward)
                ?;
        }
        if config.aim.head {
            // Head aim reads and writes the character and the input actions in the game DLL.
            let gamedll = require_build(engine::GAMEDLL, engine::GAMEDLL_SHA256)?;
            aim::look_actions_match(&gamedll).map_err(Rejection::revision)?;
            aim::resolve(&engine_module)?;
            let target = gamedll.at(engine::INPUT_ACTION);
            hooks
                .inline(&aim::INPUT_ACTION_ORIGINAL, "input actions", target, &engine::INPUT_ACTION_PROLOGUE, aim::input_action)
                ?;
        }
        if config.aim.rig || crate::research::options().arms() {
            // The arms are placed in the game DLL, against the camera, with the engine's element calls.
            let gamedll = require_build(engine::GAMEDLL, engine::GAMEDLL_SHA256)?;
            hands::resolve(&engine_module)?;
            let target = gamedll.at(engine::FPP_CAMERA_TARGET);
            hooks
                .inline(&hands::CAMERA_TARGET_ORIGINAL, "arms camera target", target, &engine::FPP_CAMERA_TARGET_PROLOGUE, hands::camera_target)
                ?;
            if let Some(physical) = config.physical_melee {
                melee::install(&mut hooks, &gamedll, &engine_module, physical, config.melee_on_reach, config.back_is_blunt)?;
            }
            // What the hands do with the world's objects, from where the rig puts them.
            hand_world::resolve(&engine_module)?;
            if config.throw_by_hand {
                throwing::install(&mut hooks, &gamedll, &engine_module)?;
            }
            if config.bow_by_hand {
                bow::install(&mut hooks, &gamedll)?;
            }
            if config.lockpick_by_hand {
                lockpick::install(&mut hooks, &gamedll)?;
            }
            if config.shoot_from_barrel {
                gun::install(&mut hooks, &gamedll)?;
            }
        }
        if config.controller_pad {
            // Optional: without it VR runs, the controllers' buttons just do nothing.
            match monaka_pad::install(&mut hooks, &channel_for_pad) {
                Ok(count) => {
                    monaka_pad::mask_real(config.pad_mask_real);
                    log!("controller pad: the VR controllers are the game's pad 0 ({count} places hooked; real pad masked: {})", config.pad_mask_real);
                }
                Err(why) => log!("controller pad off: {}", why.reason),
            }
        }
        // The probes' own hooks (each only when its option asks).
        crate::research::install(&mut hooks, &engine_module, (desc.BufferDesc.Width, desc.BufferDesc.Height))?;
        // The game's UI kept at the monitor's size (they pass through when no size switch was made).
        crate::hud::ui_size::install(&mut hooks, &engine_module, &require_build(engine::GAMEDLL, engine::GAMEDLL_SHA256)?)?;
        if config.vr_buttons {
            controls::install(&mut hooks, &require_build(engine::GAMEDLL, engine::GAMEDLL_SHA256)?, &engine_module, config.reload_gesture && config.aim.rig)?;
        }
        if config.disable_zombie_grabs {
            grabs::install(&mut hooks, &require_build(engine::GAMEDLL, engine::GAMEDLL_SHA256)?)?;
        }
        if let Some(settings) = config.room_scale {
            walk::install(&mut hooks, &engine_module)?;
            monaka_arms::roomscale::install(settings);
        }
        hooks.vtable(&stereo::PRESENT_ORIGINAL, "Present", swap.as_raw(), slot::SWAPCHAIN_PRESENT, stereo::present)?;
        if draw_hooks {
            let context = context.as_raw();
            hook_method(&mut hooks, &draws::TARGETS, "OMSetRenderTargets", context, slot::CONTEXT_OM_SET_RENDER_TARGETS, draws::set_targets)?;
            hook_method(&mut hooks, &draws::DRAW_INDEXED, "DrawIndexed", context, slot::CONTEXT_DRAW_INDEXED, draws::draw_indexed)?;
            hook_method(&mut hooks, &draws::DRAW, "Draw", context, slot::CONTEXT_DRAW, draws::draw)?;
            hook_method(&mut hooks, &draws::DRAW_INDEXED_INSTANCED, "DrawIndexedInstanced", context, slot::CONTEXT_DRAW_INDEXED_INSTANCED, draws::draw_indexed_instanced)?;
            hook_method(&mut hooks, &draws::DRAW_INSTANCED, "DrawInstanced", context, slot::CONTEXT_DRAW_INSTANCED, draws::draw_instanced)?;
            if panels::active() {
                // Where each HUD draw is placed: the matrix the game writes for it.
                hook_method(&mut hooks, &panels::MAP, "Map", context, panels::CONTEXT_MAP, panels::map)?;
                hook_method(&mut hooks, &panels::UNMAP, "Unmap", context, panels::CONTEXT_UNMAP, panels::unmap)?;
                // Which widget each HUD draw is: the game's UI elements.
                ui::install(&mut hooks, &engine_module)?;
            }
        }
    }
    stereo::DRIVER.start();
    enable_hooks(&HOOKS, hooks).inspect_err(|_| stereo::DRIVER.retire(0))
}

fn stop() -> Result<(), Rejection> {
    crate::research::report();
    monaka_arms::roomscale::stop();
    grabs::report();
    throwing::report();
    bow::report();
    gun::report();
    lockpick::report();
    hand_world::report();
    controls::stop();
    walk::restore();
    monaka_pad::stop();
    // The present thread retires the eye and gives back what the run changed.
    stereo::DRIVER.retire(2000);
    // The game applies it on its next frame; its levels then lay their menus out again.
    video::restore_and_relayout();
    remove_hooks(&HOOKS, 2000)?;
    monaka_arms::controllers::close();
    monaka_pad::report();
    monaka_pad::release();
    stereo::release();
    monaka_producer::remove_crash_log();
    Ok(())
}
