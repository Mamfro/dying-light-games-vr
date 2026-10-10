//! The game object (`IGame`): Dying Light 1 and The Beast learn it from the game DLL's calls of the
//! engine's `IGame::GetGameTimeDelta` (an import hook, [`hook`]), made every frame the game runs (not
//! while it is paused). Its video settings sit behind `[[game + 8] + offset]` ([`video_settings`];
//! the offset is each build's).

use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

pub use crate::ENGINE;
/// `float IGame::GetGameTimeDelta() const`, exported by the engine, imported by the game DLL.
pub const GAME_TIME_DELTA: &str = "?GetGameTimeDelta@IGame@@QEBAMXZ";

pub type TimeDeltaFn = unsafe extern "system" fn(game: usize) -> f32;
pub static TIME_DELTA_ORIGINAL: Original<TimeDeltaFn> = Original::new();

static GAME: AtomicUsize = AtomicUsize::new(0);
/// A producer's `fn(game)` called on each of those calls (0: none), for its probes.
static OBSERVER: AtomicUsize = AtomicUsize::new(0);

/// `IGame::GetGameTimeDelta`, as the game DLL calls it.
unsafe extern "system" fn time_delta(game: usize) -> f32 {
    let _flight = InFlight::enter();
    GAME.store(game, Relaxed);
    let observer = OBSERVER.load(Relaxed);
    if observer != 0 {
        // SAFETY: set from an `fn(usize)` by `observe`.
        unsafe { std::mem::transmute::<usize, fn(usize)>(observer)(game) };
    }
    // SAFETY: forwards the game's own call.
    unsafe { TIME_DELTA_ORIGINAL.get()(game) }
}

/// Prepares the import hook that learns the game object; nothing when it is in place already.
/// Hooked twice, the second would take the first for the game's function and keep it as the
/// original: the hook would then call itself until the thread's stack ran out.
///
/// # Safety
/// `gamedll` must be the game DLL, whose import of [`GAME_TIME_DELTA`] has [`TimeDeltaFn`]'s type.
pub unsafe fn hook(hooks: &mut Hooks, gamedll: &Module) -> monaka_hook::Result<()> {
    if hooked(gamedll) {
        return Ok(());
    }
    // SAFETY: the caller's guarantee.
    unsafe { hooks.import(&TIME_DELTA_ORIGINAL, "GetGameTimeDelta", gamedll, ENGINE, GAME_TIME_DELTA, time_delta as TimeDeltaFn) }
}

/// Whether the game DLL's import already calls the hook.
pub fn hooked(gamedll: &Module) -> bool {
    gamedll.import_slot(ENGINE, GAME_TIME_DELTA).and_then(mem::read::<usize>) == Some(time_delta as TimeDeltaFn as usize)
}

/// Has `observer` called with the game object on each frame the game runs.
pub fn observe(observer: fn(usize)) {
    OBSERVER.store(observer as usize, Relaxed);
}

/// The game object, once seen.
pub fn game() -> Option<usize> {
    Some(GAME.load(Relaxed)).filter(|&g| g != 0)
}

/// The game object, waiting up to `limit` for the game to run a frame.
pub fn wait_for_game(limit: Duration) -> Option<usize> {
    let started = Instant::now();
    while game().is_none() && started.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(5));
    }
    game()
}

/// The video settings object at `[[game + 8] + offset]`.
pub fn video_settings(game: usize, offset: usize) -> Option<usize> {
    mem::read::<usize>(mem::read::<usize>(game + 8)? + offset).filter(|&s| s != 0)
}
