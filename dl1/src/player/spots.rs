//! Named spots to come back to (debugging: the place a cutscene plays). `tools\Spot-DL1.ps1`
//! writes a command into the run folder (`spot-command.txt`: `save NAME` or `go NAME`); head aim
//! takes it on the game thread at the player's next camera update. A spot is the player's world
//! transform and head aim's facing, kept in `dl1-spots.txt` beside the run folders, so spots
//! outlive runs and game restarts.

use crate::engine::{self, GetWorldXformFn, SetWorldXformFn};
use monaka_core::camera::Mat34;
use monaka_hook::module::Module;
use monaka_hook::mem;
use monaka_hook::probe::complete_object;
use monaka_producer::{Rejection, log};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

struct Files {
    command: PathBuf,
    spots: PathBuf,
}

struct Calls {
    get: GetWorldXformFn,
    set: SetWorldXformFn,
}

static FILES: OnceLock<Files> = OnceLock::new();
static CALLS: OnceLock<Calls> = OnceLock::new();
/// When the command file was last looked at (`monaka_channel::tick`).
static CHECKED: AtomicU64 = AtomicU64::new(0);
const CHECK_EVERY_MS: u64 = 250;

/// A command for the game thread.
pub enum Command {
    Save(String),
    Go(String),
}

/// Looks up the engine's transform calls and where the files are (`run`: this run's folder).
pub fn install(engine_module: &Module, run: &Path) -> Result<(), Rejection> {
    let find = |name: &str| engine_module.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    // SAFETY: each export has the signature its mangled name states, on x64.
    let calls = unsafe {
        Calls {
            get: std::mem::transmute::<usize, GetWorldXformFn>(find(engine::GET_WORLD_XFORM)?),
            set: std::mem::transmute::<usize, SetWorldXformFn>(find(engine::SET_WORLD_XFORM)?),
        }
    };
    let _ = CALLS.set(calls);
    let spots = run.parent().unwrap_or(run).join("dl1-spots.txt");
    let _ = FILES.set(Files { command: run.join("spot-command.txt"), spots });
    Ok(())
}

/// The command waiting in the run folder, if any (looked at every [`CHECK_EVERY_MS`]); taking it
/// removes the file.
pub fn take_command(now: u64) -> Option<Command> {
    if now.saturating_sub(CHECKED.load(Relaxed)) < CHECK_EVERY_MS {
        return None;
    }
    CHECKED.store(now, Relaxed);
    let files = FILES.get()?;
    let text = std::fs::read_to_string(&files.command).ok()?;
    let _ = std::fs::remove_file(&files.command);
    let mut words = text.split_whitespace();
    match (words.next(), words.next()) {
        (Some("save"), Some(name)) => Some(Command::Save(name.to_owned())),
        (Some("go"), Some(name)) => Some(Command::Go(name.to_owned())),
        _ => {
            log!("spot: unknown command {:?}", text.trim());
            None
        }
    }
}

/// The player's `IControlObject`: at +0x18 of the complete `PlayerDI`.
fn control(character: usize) -> Option<usize> {
    Some(complete_object(character)? + engine::PLAYER_CONTROL_OBJECT)
}

/// Saves where `character` stands, with head aim's `facing` (radians), as `name`.
pub fn save(name: &str, character: usize, facing: f32) {
    let (Some(calls), Some(files), Some(control)) = (CALLS.get(), FILES.get(), control(character)) else { return };
    // SAFETY: the engine's own getter on the player's IControlObject, on the game thread.
    let at = unsafe { (calls.get)(control as *const core::ffi::c_void) };
    let Some(world) = mem::read::<Mat34>(at as usize).filter(|w| w.iter().all(|v| v.is_finite())) else {
        log!("spot {name}: the player's transform could not be read");
        return;
    };
    let mut lines: Vec<String> = std::fs::read_to_string(&files.spots).unwrap_or_default().lines().filter(|l| l.split_whitespace().next() != Some(name)).map(str::to_owned).collect();
    lines.push(format!("{name} {facing} {}", world.map(|v| v.to_string()).join(" ")));
    match std::fs::write(&files.spots, lines.join("\n") + "\n") {
        Ok(()) => log!("spot {name}: saved at [{:.2} {:.2} {:.2}], facing {:.1}", world[3], world[7], world[11], facing.to_degrees()),
        Err(why) => log!("spot {name}: not saved: {why}"),
    }
}

/// Puts `character` at the spot `name`; the facing to take on, if it was found.
pub fn go(name: &str, character: usize) -> Option<f32> {
    let (calls, files, control) = (CALLS.get()?, FILES.get()?, control(character)?);
    let text = std::fs::read_to_string(&files.spots).unwrap_or_default();
    let Some(line) = text.lines().find(|l| l.split_whitespace().next() == Some(name)) else {
        log!("spot {name}: no such spot in {}", files.spots.display());
        return None;
    };
    let numbers: Vec<f32> = line.split_whitespace().skip(1).filter_map(|w| w.parse().ok()).collect();
    let (Some(&facing), Ok(world)) = (numbers.first(), <Mat34>::try_from(numbers.get(1..).unwrap_or_default())) else {
        log!("spot {name}: the line is not a spot: {line}");
        return None;
    };
    // SAFETY: the engine's own setter on the player's IControlObject, with a matrix that lives
    // across the call, on the game thread.
    unsafe { (calls.set)(control as *mut core::ffi::c_void, world.as_ptr()) };
    log!("spot {name}: moved to [{:.2} {:.2} {:.2}]", world[3], world[7], world[11]);
    Some(facing)
}
