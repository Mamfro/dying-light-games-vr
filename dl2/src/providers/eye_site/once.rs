//! Work that does not depend on the eye, done once per frame: the right eye's scene skips it.

use super::*;

// The render script holds one JumpToHSM command per shadow map; each jumps into the shadow sub
// when its map is scheduled this frame (returning 1 continues past it). The right eye could keep
// the maps the left eye just drew, but they are fitted to the left eye (its shadows come out
// lighter), and drawing them again costs no measurable time; sharing is a debug flag only.
pub(super) unsafe extern "system" fn jump_to_hsm(command: usize, context: usize) -> usize {
    if local(|l| l.right_eye_scene.get()) && config::debug(debug::SHARE_SHADOWS) {
        SHADOW_JUMPS_SKIPPED.fetch_add(1, Ordering::Relaxed);
        return 1;
    }
    SHADOW_JUMPS_RUN.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the original, with the game's arguments.
    unsafe { JUMP_TO_HSM_ORIGINAL.get()(command, context) }
}

/// Particle simulation and sorting, wind, ray-tracing structures, heightmaps, environment probes,
/// videos and spot-light shadow maps do not depend on the eye: the right eye's scene skips these
/// commands (execute returns 1, "continue"). Several classes share an execute function, so each
/// skip is decided by the command's own vtable.
pub(super) fn skip_once(command: usize) -> bool {
    if !local(|l| l.right_eye_scene.get()) || config::debug(debug::ONCE_AGAIN) {
        return false;
    }
    // SAFETY: the engine passes a live command object, whose first field is its vtable.
    let table = unsafe { *(command as *const usize) }.wrapping_sub(game::get().engine);
    let skip = config::get().once_skip;
    engine::ONCE_COMMANDS.iter().enumerate().any(|(i, &(vtable, _, _))| vtable == table && skip & (1 << i) != 0)
}

pub(super) unsafe extern "system" fn once<const I: usize>(command: usize, b: usize, c: usize, d: usize) -> usize {
    if skip_once(command) {
        ONCE_SKIPPED.fetch_add(1, Ordering::Relaxed);
        return 1;
    }
    // SAFETY: the original, with the game's arguments.
    unsafe { ONCE_ORIGINALS[I].get()(command, b, c, d) }
}

pub(super) const ONCE_DETOURS: [OnceFn; 9] = [once::<0>, once::<1>, once::<2>, once::<3>, once::<4>, once::<5>, once::<6>, once::<7>, once::<8>];
