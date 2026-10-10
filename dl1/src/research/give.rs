//! `give=Name:count,Name:count` (testing only): gives the player items, once, a few seconds into
//! play, through the game's own new-player setup (`SessionDI::ApplyDefaultPlayerSetup`), which reads
//! `Item("name", count)` lines from a script. The script is written to this run's folder, which is
//! added to the game's file sources for it. Item names are the game's (`Bow_Composite_DeadEye`,
//! `ZZZZ_Ammo_Arrow`, `Firearm_M9Gen`, `Ammo_PistolBig`, `Throwable_ThrowingKnifeAGen`, ...).

use monaka_hook::module::Module;
use monaka_hook::mem;
use monaka_producer::log;
use std::ffi::{CString, c_char};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// The game object (`GameDI`) global; its session (`SessionDI`) is at +0x540.
const GAME: usize = 0x1c16348;
const SESSION_IN_GAME: usize = 0x540;
/// The local player global: the setup runs only while it is set.
const PLAYER: usize = 0x1c122c8;
/// `SessionDI::ApplyDefaultPlayerSetup(script path, chapter)`: applies the script's `Chapter(n)`
/// block for that chapter (1000 in The Following's levels).
const APPLY_SETUP: usize = 0x4c40b0;
const CHAPTER: u32 = 250;
const CHAPTER_FOLLOWING: u32 = 1000;
/// `bool fs::add_source(char const*, FFSAddSourceFlags)`, with the flags the engine adds its own
/// folders with.
const ADD_SOURCE: &str = "?add_source@fs@@YA_NPEBDW4ENUM@FFSAddSourceFlags@@@Z";
/// `bool fs::rem_source(char const*)`: the folder is taken out again once the items are given, so
/// nothing of the run stays among the game's file sources.
const REM_SOURCE: &str = "?rem_source@fs@@YA_NPEBD@Z";
const FILESYSTEM: &str = "filesystem_x64_rwdi.dll";
const SOURCE_FLAGS: u32 = 0x4f;
const SCRIPT: &str = "monaka_give.scr";
/// Frames of play before giving (the player settled in).
const AFTER_FRAMES: u64 = 300;

/// A `ttl::string`: the characters, their length and capacity.
#[repr(C)]
struct TtlString {
    text: *const c_char,
    length: u32,
    capacity: u32,
}

type ApplySetupFn = unsafe extern "C" fn(usize, *mut TtlString, u32);
type MallocFn = unsafe extern "C" fn(usize) -> *mut u8;
/// The C runtime the game DLL allocates from (`api-ms-win-crt-heap` forwards to it).
const CRT: &str = "ucrtbase.dll";
type AddSourceFn = unsafe extern "C" fn(*const c_char, u32) -> bool;
type RemSourceFn = unsafe extern "C" fn(*const c_char) -> bool;

static ITEMS: OnceLock<Vec<(String, u32)>> = OnceLock::new();
static FRAMES: AtomicU64 = AtomicU64::new(0);
static DONE: AtomicBool = AtomicBool::new(false);
static WAITED: AtomicBool = AtomicBool::new(false);
/// What `give` says before a game is under way.
const NOT_YET: [&str; 3] = ["no game object", "no session", "no player"];

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

/// Each game frame (on the game's thread): gives the items once.
pub fn frame(_game: usize) {
    if DONE.load(Relaxed) || FRAMES.fetch_add(1, Relaxed) < AFTER_FRAMES {
        return;
    }
    match give() {
        Ok(()) => {
            DONE.store(true, Relaxed);
            log!("give: done");
        }
        // Not in a game yet (the menus, a load): tried again a little later.
        Err(why) if NOT_YET.contains(&why.as_str()) => {
            FRAMES.store(0, Relaxed);
            if !WAITED.swap(true, Relaxed) {
                log!("give: waiting for a game ({why})");
            }
        }
        Err(why) => {
            DONE.store(true, Relaxed);
            log!("give: not given: {why}");
        }
    }
}

fn give() -> Result<(), String> {
    let items = ITEMS.get().ok_or("no items")?;
    let gamedll = Module::find(crate::engine::GAMEDLL).ok_or("no game DLL")?;
    let game = mem::read::<usize>(gamedll.at(GAME)).filter(|&g| g != 0).ok_or("no game object")?;
    let session = mem::read::<usize>(game + SESSION_IN_GAME).filter(|&s| s != 0).ok_or("no session")?;
    if mem::read::<usize>(gamedll.at(PLAYER)).is_none_or(|p| p == 0) {
        return Err("no player".into());
    }
    log!(
        "give: game {game:#x} ({}), session {session:#x} ({})",
        monaka_hook::probe::class_name(game).unwrap_or_default(),
        monaka_hook::probe::class_name(session).unwrap_or_default()
    );
    // `SessionDI` or a class built on it first (`SessionCooperativeDI`, in ordinary play).
    if !monaka_hook::probe::class_name(session).is_some_and(|c| c.contains("SessionDI") || c.contains("SessionCooperativeDI")) {
        return Err("the session is not a SessionDI".into());
    }

    // The script, in a folder of its own beside the log.
    let folder = super::dir().join("give");
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let lines: String = items.iter().map(|(name, count)| format!("        Item(\"{name}\", {count});\n")).collect();
    let block = |chapter: u32| format!("    Chapter({chapter})\n    {{\n{lines}    }}\n");
    let script = format!("sub main()\n{{\n{}{}}}\n", block(CHAPTER), block(CHAPTER_FOLLOWING));
    std::fs::write(folder.join(SCRIPT), script).map_err(|e| e.to_string())?;

    let filesystem = Module::find(FILESYSTEM).ok_or("no file system DLL")?;
    let add_source = filesystem.export(ADD_SOURCE).ok_or("no fs::add_source")?;
    let source = format!("{}/", folder.to_string_lossy().replace('\\', "/"));
    let source = CString::new(source).map_err(|e| e.to_string())?;
    // SAFETY: the file system's export, with a folder path and the engine's own flags for folders.
    let added = unsafe { std::mem::transmute::<usize, AddSourceFn>(add_source)(source.as_ptr(), SOURCE_FLAGS) };
    log!("give: added {source:?} as a file source: {added}");

    // The setup frees the path's characters when done: they come from the C runtime the game DLL
    // allocates with (the shared `ucrtbase.dll`; this DLL's own runtime is linked in).
    let malloc = Module::find(CRT).and_then(|m| m.export("malloc")).ok_or("no ucrtbase malloc")?;
    // SAFETY: the C runtime's `malloc`.
    let text = unsafe { std::mem::transmute::<usize, MallocFn>(malloc)(SCRIPT.len() + 1) };
    if text.is_null() {
        return Err("malloc failed".into());
    }
    // SAFETY: `text` holds `SCRIPT.len() + 1` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(SCRIPT.as_ptr(), text, SCRIPT.len());
        *text.add(SCRIPT.len()) = 0;
    }
    let mut path = TtlString { text: text as *const c_char, length: SCRIPT.len() as u32, capacity: SCRIPT.len() as u32 };
    let chapter = CHAPTER;
    log!("give: {items:?}");
    // SAFETY: the game's own setup on its live session, on the game thread, with a script path
    // allocated as it frees it; it checks the player itself and logs a script it cannot read.
    unsafe { std::mem::transmute::<usize, ApplySetupFn>(gamedll.at(APPLY_SETUP))(session, &mut path, chapter) };
    if let Some(rem_source) = filesystem.export(REM_SOURCE) {
        // SAFETY: the file system's export, with the path the folder was added by.
        let removed = unsafe { std::mem::transmute::<usize, RemSourceFn>(rem_source)(source.as_ptr()) };
        log!("give: took the folder out of the file sources again: {removed}");
    }
    Ok(())
}
