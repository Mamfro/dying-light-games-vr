//! The loaded game modules and the engine functions the stereo pair calls, set once at start
//! before any hook runs.

use crate::engine::{self, ComponentSetFn, EnterFn, LeaveFn, LevelFn, RebuildFn};
use monaka_hook::mem;
use std::sync::OnceLock;

pub struct Game {
    pub engine: usize,
    pub gamedll: Option<usize>,
    pub renderer11: Option<usize>,
    pub renderer12: Option<usize>,
    pub component_set: ComponentSetFn,
    pub rebuild_projection: RebuildFn,
    pub reset_level: LevelFn,
    pub renderer_enter: EnterFn,
    pub renderer_leave: LeaveFn,
}

static GAME: OnceLock<Game> = OnceLock::new();

pub fn set(game: Game) {
    let _ = GAME.set(game);
}

/// The game, once started (every hook runs after that).
pub fn get() -> &'static Game {
    GAME.get().expect("hooks run only after the start set the game")
}

pub fn try_get() -> Option<&'static Game> {
    GAME.get()
}

impl Game {
    pub fn engine_at(&self, rva: usize) -> usize {
        self.engine + rva
    }

    /// The engine's game object and its frame counter.
    pub fn counter(&self) -> Option<(usize, u32)> {
        let game = mem::read::<usize>(self.engine_at(engine::GAME_GLOBAL)).filter(|&g| g != 0)?;
        Some((game, mem::read::<u32>(game + engine::GAME_COUNTER)?))
    }
}

/// Reads a camera's state (camera-to-world, projection, frustum); none for anything that is not
/// a live engine camera (a menu or loading scene may prepare none).
pub fn read_camera(camera: usize) -> Option<([f32; 12], [f32; 16], [f32; 12])> {
    if camera == 0 || mem::read::<usize>(camera)? != get().engine_at(engine::CAMERA_VTABLE) {
        return None;
    }
    Some((
        mem::read(camera + engine::CAMERA_INVERSE)?,
        mem::read(camera + engine::CAMERA_PROJECTION)?,
        mem::read(camera + engine::CAMERA_FRUSTUM)?,
    ))
}

/// Milliseconds since boot.
pub fn tick() -> u64 {
    monaka_channel::tick()
}
