//! The HUD and menus: placed in front of each eye at the back-buffer draws ([`draws`]), its pieces
//! cut onto the hands' panels by draw ([`panels`]), matched to the widgets of the game's UI tree
//! ([`ui`]), the UI kept at the monitor's size ([`ui_size`]).

pub(crate) mod draws;
pub(crate) mod panels;
pub(crate) mod ui;
pub(crate) mod ui_size;

use crate::config::Config;
use crate::{Dl1, hook_method, research, stereo, swapchain_parts};
use monaka_core::feature::Feature;
use monaka_core::manifest::{HUD_DISTANCE, HUD_SIZE};
use monaka_hook::slot;
use monaka_producer::game::{Component, Lack, On, Start};
use windows::core::Interface;

/// The back buffer's draws tracked on the immediate context ([`draws`]): for the HUD, the
/// upscaler's and the hybrid's scene copy, and the probes.
pub const DRAWS: Component<Dl1> = Component {
    name: "draws",
    needs: &[&stereo::PRESENT],
    on: |run| {
        let config = Config::from_options(run.options, run.manifest);
        On::when(config.hud_active() || config.hybrid || config.fsr.is_some() || research::options().draws(), "no draws to follow")
    },
    install: install_draws,
    ..Component::NONE
};

fn install_draws(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = crate::config(start);
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
/// world's get a viewport at the HUD's place in each eye ([`draws`]).
pub const HUD: Component<Dl1> = Component {
    name: "hud",
    features: &[Feature::HudLayer],
    options: &[HUD_SIZE.default_number(0.36), HUD_DISTANCE],
    needs: &[&DRAWS],
    on: |run| On::when(run.shared.hud.shown, "the HUD stays as the game draws it"),
    ..Component::NONE
};

/// The dynamic HUD ([`panels`]): each HUD piece's draws, told apart by the game's UI elements
/// ([`ui`]) and placed by the matrix the game maps for it, to the panels on the hands.
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

/// The game's UI kept at the monitor's size while it renders at the eye size ([`ui_size`]; it
/// passes through when no size switch was made).
pub const UI_SIZE: Component<Dl1> = Component {
    name: "ui size",
    needs: &[&stereo::PRESENT],
    install: |start| Ok(ui_size::install(start.hooks, &start.build.engine, &start.build.gamedll)?),
    ..Component::NONE
};
