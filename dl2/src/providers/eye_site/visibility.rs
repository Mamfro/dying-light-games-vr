//! Visibility follows the tracked eye, not the mouse camera: the right eye's scene culls by its own
//! camera.

use super::*;

pub(super) unsafe extern "system" fn view_setup(level: usize) -> usize {
    let _guard = InFlight::enter();
    let previous = local(|l| l.visibility_setup_level.replace(level));
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { VIEW_SETUP_ORIGINAL.get()(level) };
    local(|l| l.visibility_setup_level.set(previous));
    result
}

pub(super) fn tracked_camera() -> Option<usize> {
    local(|l| (l.stereo_repeat.get() && l.tracked_visibility_camera.get() != 0).then(|| l.tracked_visibility_camera.get()))
}

monaka_hook::caller_shim!(visibility_camera_stub => visibility_camera);

/// The main visibility setup switches to engine context 1 here, selecting the mouse camera instead
/// of the tracked one; only that call is redirected, for the stereo level.
pub(super) unsafe extern "system" fn visibility_camera(wrapper: usize, _: usize, _: usize, caller: usize) -> usize {
    let _guard = InFlight::enter();
    // SAFETY: the original, with the game's arguments.
    let original = unsafe { VISIBILITY_CAMERA_ORIGINAL.get()(wrapper) };
    match tracked_camera() {
        Some(tracked) if caller == game::get().engine_at(engine::VISIBILITY_CAMERA_CALLER) && wrapper + 0x30 == tracked => tracked,
        _ => original,
    }
}

monaka_hook::caller_shim!(base_camera_copy_stub => base_camera_copy);

pub(super) unsafe extern "system" fn base_camera_copy(output: usize, mut source: usize, _: usize, caller: usize) -> usize {
    let _guard = InFlight::enter();
    if caller == game::get().engine_at(engine::BASE_CAMERA_COPY_CALLER)
        && let Some(tracked) = tracked_camera()
        && local(|l| l.visibility_setup_level.get()) + engine::LEVEL_CAMERA == tracked
    {
        source = tracked;
    }
    // SAFETY: the original, with the game's arguments (the source possibly the tracked camera).
    unsafe { BASE_CAMERA_COPY_ORIGINAL.get()(output, source) }
}

/// A visibility result supplied from the mouse camera would cull what the turned head can see.
/// Four arguments, so no shim: the (slower) stack walk, only while the right eye renders.
pub(super) unsafe extern "system" fn main_visibility(manager: usize, view: u32, camera: usize, mut input: usize) -> usize {
    let _guard = InFlight::enter();
    if view == 20 && tracked_camera().is_some() && monaka_hook::probe::caller() == game::get().engine_at(engine::MAIN_VISIBILITY_CALLER) {
        input = 0;
    }
    // SAFETY: the original, with the game's arguments (no supplied visibility input).
    unsafe { MAIN_VISIBILITY_ORIGINAL.get()(manager, view, camera, input) }
}

monaka_hook::caller_shim!(world_visibility_stub => world_visibility);

pub(super) unsafe extern "system" fn world_visibility(world: usize, camera: usize, mut input: usize, caller: usize) -> usize {
    let _guard = InFlight::enter();
    if caller == game::get().engine_at(engine::WORLD_VISIBILITY_CALLER) && local(|l| l.stereo_repeat.get()) && camera == local(|l| l.tracked_visibility_camera.get()) {
        input = 0;
    }
    // SAFETY: as above.
    unsafe { WORLD_VISIBILITY_ORIGINAL.get()(world, camera, input) }
}
