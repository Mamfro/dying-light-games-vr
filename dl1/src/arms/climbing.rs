//! Whether the player climbs, from the game itself: its movement controllers (hanging from or
//! climbing a ledge, a ladder, a zip line) each have an id, and the player keeps a table of which
//! run (a byte per id at `[player + 0xb98]`, set by the controller's start and cleared by its end).
//! The hand rig is told each update ([`monaka_arms::traverse::report_climbing`]): the arms go to
//! the game's animation exactly while one of them runs, whatever else raises the hands (a throw's
//! wind-up).

use monaka_hook::mem;

/// In the player: its table of running controllers, a byte per controller id.
const RUNNING: usize = 0xb98;
/// The controllers that climb: `LadderController`, `EdgeGrabController` (ledges, climbing a wall
/// ledge by ledge), `ZipLineController`.
const LADDER: usize = 0x07;
const LEDGE: usize = 0x18;
const ZIP_LINE: usize = 0x21;
const CLIMBING: [usize; 3] = [LADDER, LEDGE, ZIP_LINE];

/// Whether `player` (the arms model, which is the player object) runs a climbing controller now.
pub fn climbing(player: usize) -> Option<bool> {
    if player == 0 {
        return None;
    }
    let table = mem::read::<usize>(player + RUNNING).filter(|&t| t != 0)?;
    Some(CLIMBING.iter().any(|&id| mem::read::<u8>(table + id).is_some_and(|b| b != 0)))
}
