//! `probe_melee`: every hit the game deals, as it reaches the engine's
//! `IControlObject::TakeDamage(SDamageInfo const&)`. The engine clones the damage info there and
//! dispatches it, so one hook sees all damage. Logs the victim's class, the call chain up from the
//! game DLL (which function built the hit), and the damage info's bytes as hex, floats and ints, to
//! read its layout from hits landed on known places from known directions. Read-only.

use monaka_hook::module::Module;
use monaka_hook::probe::class_name;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use windows::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;

const TAKE_DAMAGE: &str = "?TakeDamage@IControlObject@@UEAAXAEBUSDamageInfo@@@Z";
/// How much of the damage info is logged (its full size is not known).
const DUMPED: usize = 0x180;
/// Hits logged in full; later ones get one line.
const FULL: u64 = 40;

type TakeDamageFn = unsafe extern "C" fn(*mut c_void, *const u8);

static ORIGINAL: Original<TakeDamageFn> = Original::new();
static HITS: AtomicU64 = AtomicU64::new(0);
static ATTRACTOR_HITS: AtomicU64 = AtomicU64::new(0);
static INSTALLED: AtomicBool = AtomicBool::new(false);

pub fn install(hooks: &mut Hooks, engine: &Module) -> Result<(), Rejection> {
    let target = engine.export(TAKE_DAMAGE).ok_or_else(|| Rejection::revision("the engine does not export IControlObject::TakeDamage"))?;
    // SAFETY: the detour has the export's signature; its prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&ORIGINAL, "IControlObject::TakeDamage", target, take_damage as TakeDamageFn)? };
    INSTALLED.store(true, Relaxed);
    log!("melee probe: watching IControlObject::TakeDamage");
    Ok(())
}

unsafe extern "C" fn take_damage(victim: *mut c_void, info: *const u8) {
    let _flight = InFlight::enter();
    // The game damages its sound and noise attractors every second or so; only counted.
    if class_name(victim as usize).as_deref() == Some(".?AVVirtualAttractor@@") {
        ATTRACTOR_HITS.fetch_add(1, Relaxed);
        // SAFETY: forwards the game's own call.
        return unsafe { ORIGINAL.get()(victim, info) };
    }
    let n = HITS.fetch_add(1, Relaxed);
    if !info.is_null() && n < FULL {
        record(n, victim as usize, info as usize);
    } else if n < 400 {
        let kind = mem::read::<u32>(info as usize + 0x3c).unwrap_or(u32::MAX);
        log!("melee probe: hit {n} on {victim:p} type {kind}");
    }
    // SAFETY: forwards the game's own call.
    unsafe { ORIGINAL.get()(victim, info) }
}

pub fn report() {
    if !INSTALLED.load(Relaxed) {
        return;
    }
    log!("melee probe: {} hits logged, {} on attractors skipped", HITS.load(Relaxed), ATTRACTOR_HITS.load(Relaxed));
}

fn record(n: u64, victim: usize, info: usize) {
    // The control object's owner (its +8) is the game object the game DLL subclasses.
    let owner = mem::read::<usize>(victim + 8).unwrap_or(0);
    let mut frames = [std::ptr::null_mut(); 12];
    // SAFETY: fills our buffer with return addresses.
    let captured = unsafe { RtlCaptureStackBackTrace(1, &mut frames, None) } as usize;
    let chain: Vec<String> = frames[..captured].iter().map(|&f| Module::describe(f as usize)).collect();
    log!(
        "melee probe: hit {n} victim {victim:#x} ({}) owner {owner:#x} ({}) info {info:#x} ({})",
        class(victim),
        class(owner),
        class(info)
    );
    log!("melee probe: hit {n} called from {}", chain.join(" < "));
    let mut bytes = [0u8; DUMPED];
    if !mem::read_bytes(info, &mut bytes) {
        log!("melee probe: hit {n} info unreadable");
        return;
    }
    for (row, chunk) in bytes.chunks(16).enumerate() {
        let words: Vec<[u8; 4]> = chunk.chunks(4).map(|w| [w[0], w[1], w[2], w[3]]).collect();
        let floats: Vec<String> = words.iter().map(|w| format!("{:>10.3}", f32::from_le_bytes(*w))).collect();
        let ints: Vec<String> = words.iter().map(|w| format!("{:>11}", i32::from_le_bytes(*w))).collect();
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        log!("melee probe: hit {n} +{:03x} {} | {} | {}", row * 16, hex.join(" "), floats.join(" "), ints.join(" "));
    }
}

fn class(object: usize) -> String {
    let vtable = mem::read::<usize>(object).unwrap_or(0);
    format!("{} vtable {}", class_name(object).unwrap_or_else(|| "?".into()), Module::describe(vtable))
}
