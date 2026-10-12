//! `give=Name:count,Name:count` (testing only): gives the player items, once, a few seconds into
//! play, through the game's own give-item (what the story's item rewards call): by name, added to
//! the inventory, nothing taken away. Item names are the game's (`Throwable_ThrowingKnife_FT_T1`,
//! `Throwable_Molotov_FT_T1`, `dlc_ft_wpn_lbow_h_r1`, `Ammo_Arrow`, `dlc_ft_firearm_pistol_b_military_r1`,
//! `Ammo_Firearm_Pistol`, ...; any case); an unknown one is refused and logged. Single player only:
//! in co-op the game hands rewards out through the session instead.

use monaka_hook::mem;
use monaka_hook::module::Module;
use monaka_producer::log;
use std::ffi::CString;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// The game object global.
const GAME: usize = 0x3acb4f0;
/// The local player `(game) -> player or 0` (0 without a level or a player).
const LOCAL_PLAYER: usize = 0x15ac280;
/// Gives an item: `(the player's +0x3be8, player, name, count, force, out description, out flag,
/// equip, to the stash) -> the name was known`. The flags left off keep the weapon in hand.
const GIVE: usize = 0x116f710;
const GIVE_FIRST: usize = 0x3be8;
/// A string the callee only reads: the text's address with this tag in the top byte.
const BORROWED: u64 = 0x1100_0000_0000_0000;
const ADDRESS: u64 = 0x00ff_ffff_ffff_ffff;
/// Frames of play before giving (the player settled in).
const AFTER_FRAMES: u64 = 300;

type PlayerFn = unsafe extern "C" fn(usize) -> usize;
type GiveFn = unsafe extern "C" fn(usize, usize, *const u64, i32, bool, usize, usize, bool, bool) -> bool;

static ITEMS: OnceLock<Vec<(String, i32)>> = OnceLock::new();
static FRAMES: AtomicU64 = AtomicU64::new(0);
static DONE: AtomicBool = AtomicBool::new(false);
static WAITED: AtomicBool = AtomicBool::new(false);

/// Reads `give=Name:count,...` (a name alone: one).
pub fn configure(text: &str) {
    let items = text
        .split(',')
        .filter_map(|part| {
            let (name, count) = part.split_once(':').unwrap_or((part, "1"));
            let name = name.trim();
            let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            ok.then(|| (name.to_owned(), count.trim().parse().unwrap_or(1)))
        })
        .collect();
    let _ = ITEMS.set(items);
}

pub fn wanted() -> bool {
    ITEMS.get().is_some_and(|items| !items.is_empty())
}

/// Each game frame (on the game's thread): gives the items once, when there is a player. The
/// give-item itself reads the frame time, which calls back in here: [`DONE`] is set before it runs.
pub fn frame(_game: usize) {
    if DONE.load(Relaxed) || FRAMES.fetch_add(1, Relaxed) < AFTER_FRAMES {
        return;
    }
    match give() {
        Ok(()) => {}
        // Not in a game yet (the menus, a load): tried again a little later.
        Err(why) => {
            FRAMES.store(0, Relaxed);
            if !WAITED.swap(true, Relaxed) {
                log!("give: waiting for a game ({why})");
            }
        }
    }
}

fn give() -> Result<(), &'static str> {
    let items = ITEMS.get().ok_or("no items")?;
    let gamedll = Module::find(crate::providers::engine::GAMEDLL).ok_or("no game DLL")?;
    let game = mem::read::<usize>(gamedll.at(GAME)).filter(|&g| g != 0).ok_or("no game object")?;
    // SAFETY: the game's own local-player getter on its game object, on the game thread.
    let player = unsafe { std::mem::transmute::<usize, PlayerFn>(gamedll.at(LOCAL_PLAYER))(game) };
    if player == 0 || mem::read::<usize>(player) != Some(gamedll.at(crate::providers::engine::PLAYER_VTABLE)) {
        return Err("no player");
    }
    let first = mem::read::<usize>(player + GIVE_FIRST).ok_or("no player")?;
    let give = gamedll.at(GIVE);
    // Once, whatever comes of it: the calls below come back into `frame`.
    if DONE.swap(true, Relaxed) {
        return Ok(());
    }
    for (name, count) in items {
        let Ok(text) = CString::new(name.as_str()) else { continue };
        let borrowed = (text.as_ptr() as u64 & ADDRESS) | BORROWED;
        // SAFETY: the game's give-item on its live local player, on the game thread, as the
        // story's rewards call it: the name is read during the call only, every flag off.
        let known = unsafe { std::mem::transmute::<usize, GiveFn>(give)(first, player, &borrowed, *count, false, 0, 0, false, false) };
        log!("give: {name} x{count}: {}", if known { "given" } else { "unknown name, not given" });
    }
    Ok(())
}
