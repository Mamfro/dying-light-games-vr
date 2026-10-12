//! Part of the HUD pieces provider ([`crate::providers::hud_pieces`]): which UI element each UI
//! draw is, told by the renderer itself as it draws.
//!
//! The UI is drawn from a queue: walking the UI tree, each drawing element queues itself, and the
//! renderer replays the queue later, handing each item to its renderable's `SetupDynRendering`,
//! which fills the item's vertices and draws them before it returns. So while a setup runs, its
//! draws are its element's, and nothing else is drawn:
//! - an image, rectangle, shape or effect group is its own renderable, a part of the element
//!   ([`engine::UI_RENDERABLE`] into it);
//! - a text is drawn by a panel text of its own, which holds its element
//!   ([`engine::PANEL_TEXT_ELEMENT`]).
//!
//! [`drawing`] is that element while its setup runs on this thread (the engine object an
//! `IUIElement` wraps, as [`crate::providers::hud_widgets`] keys the tree's elements), else 0.

use crate::providers::engine::{self, UiSetupFn};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::game::Lack;
use std::cell::Cell;

thread_local! {
    /// The element whose setup runs on this thread now (0: none).
    static DRAWING: Cell<usize> = const { Cell::new(0) };
}

static ELEMENT_ORIGINALS: [Original<UiSetupFn>; engine::UI_ELEMENT_SETUPS.len()] = [const { Original::new() }; engine::UI_ELEMENT_SETUPS.len()];
static PANEL_TEXT_ORIGINAL: Original<UiSetupFn> = Original::new();

/// Hooks the UI renderables' setups in the engine.
pub fn install(hooks: &mut Hooks, engine_module: &Module) -> Result<(), Lack> {
    let detours: [UiSetupFn; engine::UI_ELEMENT_SETUPS.len()] = [element_setup::<0>, element_setup::<1>, element_setup::<2>, element_setup::<3>, element_setup::<4>];
    for ((original, site), detour) in ELEMENT_ORIGINALS.iter().zip(&engine::UI_ELEMENT_SETUPS).zip(detours) {
        hooks.site(original, engine_module, site, detour)?;
    }
    hooks.site(&PANEL_TEXT_ORIGINAL, engine_module, &engine::PANEL_TEXT_SETUP, panel_text_setup)?;
    Ok(())
}

/// The UI element being drawn on this thread now: the engine object, or 0 outside every UI
/// element's setup.
pub fn drawing() -> usize {
    DRAWING.with(Cell::get)
}

/// Runs a setup with `element` as the one being drawn, then puts back the one before (setups do not
/// nest, but a stray one must not leave its element behind).
fn drawn_as(element: usize, setup: impl FnOnce() -> usize) -> usize {
    let before = DRAWING.with(|d| d.replace(element));
    let result = setup();
    DRAWING.with(|d| d.set(before));
    result
}

/// An element's own setup, the `I`th of [`engine::UI_ELEMENT_SETUPS`].
unsafe extern "system" fn element_setup<const I: usize>(renderable: usize, context: usize, item: usize, count: i32) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: forwards the renderer's own call.
    drawn_as(renderable.wrapping_sub(engine::UI_RENDERABLE), || unsafe { ELEMENT_ORIGINALS[I].get()(renderable, context, item, count) })
}

/// A text element's panel text's setup.
unsafe extern "system" fn panel_text_setup(panel: usize, context: usize, item: usize, count: i32) -> usize {
    let _flight = InFlight::enter();
    let element = mem::read::<usize>(panel + engine::PANEL_TEXT_ELEMENT).unwrap_or(0);
    // SAFETY: forwards the renderer's own call.
    drawn_as(element, || unsafe { PANEL_TEXT_ORIGINAL.get()(panel, context, item, count) })
}
