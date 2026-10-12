//! Provider, to be: settings (a game setting changed and put back): the head bob held at 0, with
//! the camera update it is held at.
//!
//! Disabled view bobbing (`disable_view_bobbing`, a setting on by default): while VR runs the player's
//! `HeadBobFactor` parameter is held at 0, and the game's own value is put back when VR stops.
//! The game blends the camera between its bobbing place (the steps' rise and fall) and a steady one
//! by that factor, and moves the first-person arms with it: at 0 the view stays level through
//! walking, running and landing, as the player's own head is. Held each camera update ([`hold`]),
//! as the game may load its parameters again (a level change).

use monaka_hook::mem;
use monaka_hook::module::Module;
use monaka_producer::log;
use std::sync::{Mutex, OnceLock};

/// One build's player parameters (game DLL RVA and offsets).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
    /// Its parameter block's getter: vtable slot `() -> block`.
    pub parameters_slot: usize,
    /// `HeadBobFactor` (f32) in the block.
    pub head_bob: usize,
}

/// The build, with the player vtable relocated.
static BUILD: OnceLock<Build> = OnceLock::new();
/// The block held, and the game's factor in it before.
static SAVED: Mutex<Option<(usize, f32)>> = Mutex::new(None);

pub fn install(gamedll: &Module, build: Build) {
    let _ = BUILD.set(Build { player_vtable: gamedll.at(build.player_vtable), ..build });
    log!("view bobbing: off while VR runs (disable_view_bobbing=0 keeps it)");
}

/// One camera update of `character`: the factor held at 0 while `on` (VR running), the game's put
/// back once it is not.
pub fn hold(character: usize, on: bool) {
    let Some(build) = BUILD.get() else { return };
    if mem::read::<usize>(character) != Some(build.player_vtable) {
        return;
    }
    let Some(getter) = mem::read::<usize>(build.player_vtable + build.parameters_slot).filter(|&g| g != 0) else { return };
    // SAFETY: the player's own parameter getter, no arguments beyond it, on the game thread, as the
    // game's camera code calls it.
    let block = unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(usize) -> usize>(getter)(character) };
    if block == 0 {
        return;
    }
    let at = block + build.head_bob;
    let Ok(mut saved) = SAVED.lock() else { return };
    if on {
        let Some(factor) = mem::read::<f32>(at).filter(|f| f.is_finite()) else { return };
        if factor != 0.0 {
            if saved.is_none_or(|(b, _)| b != block) {
                *saved = Some((block, factor));
                log!("view bobbing: held off (the game's factor was {factor})");
            }
            mem::write(at, 0.0f32);
        }
    } else if let Some((b, factor)) = *saved
        && b == block
    {
        mem::write(at, factor);
        *saved = None;
    }
}
