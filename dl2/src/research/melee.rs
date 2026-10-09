//! Melee probes (mapped from DL1's).
//!
//! - `probe_melee=1`: every damage message the game sends (`0x215bd40(system, victim, &info,
//!   flag)`, DL2's stand-in for DL1's exported `TakeDamage`): the victim's class, the call chain up
//!   the game DLL and the info's bytes as hex, floats and ints.
//! - `probe_sweep=1`: the melee controller's hit detection. Each frame of the hit phase
//!   (`0xd59ed0(player, controller+0x40, track, hand)`): the attack type, the window, the phase,
//!   the direction override and the swept blade (start, tip, length, how far the start moved since
//!   the last frame); and each hit it deals (`0xd58050(helper, &result, &info)`): bone, element,
//!   point (and how far it is along and off the blade), direction and side.
//!
//! Read-only.

use monaka_hook::module::Module;
use monaka_hook::probe::class_name;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use std::time::Instant;
use windows::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;

/// Sends a damage message to its victim.
const SEND_DAMAGE: usize = 0x215bd40;
/// One frame of the melee controller's hit detection.
const DETECT: usize = 0xd59ed0;
/// Deals one hit from its damage info.
const DEAL: usize = 0xd58050;

/// The melee controller's `IMeleeController` interface sits here in it.
const INTERFACE: usize = 0x40;
const ATTACK_TYPE: usize = 0x150;
const DIRECTION: usize = 0x1bc;
const WINDOW: usize = 0x1c8;
const STARTED: usize = 0x1d0;
const PHASE: usize = 0x468;
/// The track: current blade start and tip, then the previous ones.
const TRACK_START: usize = 0x00;
const TRACK_TIP: usize = 0x0c;
const TRACK_LAST_START: usize = 0x18;

/// How much of a damage info is dumped (its size is not known).
const DUMPED: usize = 0x100;
const FULL: u64 = 30;
const LINES: u64 = 600;

type SendDamageFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *const u8, usize) -> usize;
type DetectFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, usize) -> usize;
type DealFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *const u8) -> usize;

static SEND_DAMAGE_ORIGINAL: Original<SendDamageFn> = Original::new();
static DETECT_ORIGINAL: Original<DetectFn> = Original::new();
static DEAL_ORIGINAL: Original<DealFn> = Original::new();
static INSTALLED: AtomicBool = AtomicBool::new(false);
static DAMAGES: AtomicU64 = AtomicU64::new(0);
static SWEEPS: AtomicU64 = AtomicU64::new(0);
static HITS: AtomicU64 = AtomicU64::new(0);
static STARTED_AT: Mutex<Option<Instant>> = Mutex::new(None);
/// A swept blade: where it starts, which way it runs (unit) and how long it is.
type Blade = ([f32; 3], [f32; 3], f32);
/// The latest swept blade, for placing hits on it.
static BLADE: Mutex<Option<Blade>> = Mutex::new(None);

pub fn install(hooks: &mut Hooks, gamedll: &Module, damage: bool, sweep: bool) -> Result<(), Rejection> {
    *STARTED_AT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    // SAFETY: the detours have the targets' signatures (from their code and call sites); the game
    // DLL's build is checked by the caller; prologues are decoded and moved.
    unsafe {
        if damage {
            hooks.inline_decoded(&SEND_DAMAGE_ORIGINAL, "damage message", gamedll.at(SEND_DAMAGE), send_damage as SendDamageFn)?;
        }
        if sweep {
            hooks.inline_decoded(&DETECT_ORIGINAL, "melee hit detection", gamedll.at(DETECT), detect as DetectFn)?;
            hooks.inline_decoded(&DEAL_ORIGINAL, "melee hit dealing", gamedll.at(DEAL), deal as DealFn)?;
        }
    }
    INSTALLED.store(true, Relaxed);
    log!("melee probe: damage messages {damage}, hit detection {sweep}");
    Ok(())
}

fn ms() -> u128 {
    STARTED_AT.lock().ok().and_then(|s| *s).map_or(0, |s| s.elapsed().as_millis())
}

fn vec3(at: usize) -> [f32; 3] {
    mem::read::<[f32; 3]>(at).unwrap_or([f32::NAN; 3])
}

fn class(object: usize) -> String {
    class_name(object).unwrap_or_else(|| "?".into())
}

unsafe extern "C" fn send_damage(system: *mut c_void, victim: *mut c_void, info: *const u8, flag: usize) -> usize {
    let _flight = InFlight::enter();
    let n = DAMAGES.fetch_add(1, Relaxed);
    if n < FULL && !info.is_null() {
        let mut frames = [std::ptr::null_mut(); 10];
        // SAFETY: fills our buffer with return addresses.
        let captured = unsafe { RtlCaptureStackBackTrace(1, &mut frames, None) } as usize;
        let chain: Vec<String> = frames[..captured].iter().map(|&f| Module::describe(f as usize)).collect();
        log!("melee probe: {} ms damage {n} to {victim:p} ({}) info {info:p} ({}) flag {flag:#x}", ms(), class(victim as usize), class(info as usize));
        log!("melee probe: damage {n} sent from {}", chain.join(" < "));
        let mut bytes = [0u8; DUMPED];
        if mem::read_bytes(info as usize, &mut bytes) {
            for (row, chunk) in bytes.chunks(16).enumerate() {
                let words: Vec<[u8; 4]> = chunk.chunks(4).map(|w| [w[0], w[1], w[2], w[3]]).collect();
                let floats: Vec<String> = words.iter().map(|w| format!("{:>10.3}", f32::from_le_bytes(*w))).collect();
                let ints: Vec<String> = words.iter().map(|w| format!("{:>11}", i32::from_le_bytes(*w))).collect();
                let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                log!("melee probe: damage {n} +{:03x} {} | {} | {}", row * 16, hex.join(" "), floats.join(" "), ints.join(" "));
            }
        }
    } else if n < LINES {
        log!("melee probe: {} ms damage {n} to {victim:p} ({})", ms(), class(victim as usize));
    }
    // SAFETY: forwards the game's own call.
    unsafe { SEND_DAMAGE_ORIGINAL.get()(system, victim, info, flag) }
}

unsafe extern "C" fn detect(player: *mut c_void, interface: *mut c_void, track: *mut c_void, hand: usize) -> usize {
    let _flight = InFlight::enter();
    let n = SWEEPS.fetch_add(1, Relaxed);
    let (track, ctrl) = (track as usize, (interface as usize).wrapping_sub(INTERFACE));
    let start = vec3(track + TRACK_START);
    let tip = vec3(track + TRACK_TIP);
    let along = [tip[0] - start[0], tip[1] - start[1], tip[2] - start[2]];
    let length = (along[0] * along[0] + along[1] * along[1] + along[2] * along[2]).sqrt();
    if length > 1e-4 && length.is_finite() {
        *BLADE.lock().unwrap_or_else(|e| e.into_inner()) = Some((start, along.map(|c| c / length), length));
    }
    if n < LINES {
        let last = vec3(track + TRACK_LAST_START);
        let moved = ((start[0] - last[0]).powi(2) + (start[1] - last[1]).powi(2) + (start[2] - last[2]).powi(2)).sqrt();
        log!(
            "melee probe: {} ms sweep {n} player {player:p} ({}) hand {hand} | type {} window {:.3?} started {:.3} phase {} direction {:.2?} | blade from {start:.3?} {length:.2} m long, start moved {moved:.3} m",
            ms(),
            class(player as usize),
            mem::read::<i32>(ctrl + ATTACK_TYPE).unwrap_or(-1),
            mem::read::<[f32; 2]>(ctrl + WINDOW).unwrap_or([f32::NAN; 2]),
            mem::read::<f32>(ctrl + STARTED).unwrap_or(f32::NAN),
            mem::read::<i32>(ctrl + PHASE).unwrap_or(-1),
            vec3(ctrl + DIRECTION),
        );
    }
    // SAFETY: forwards the game's own call.
    unsafe { DETECT_ORIGINAL.get()(player, interface, track as *mut c_void, hand) }
}

unsafe extern "C" fn deal(helper: *mut c_void, result: *mut c_void, info: *const u8) -> usize {
    let _flight = InFlight::enter();
    let n = HITS.fetch_add(1, Relaxed);
    if n < LINES && !info.is_null() {
        let at = info as usize;
        let victim = mem::read::<usize>(at + 0x08).unwrap_or(0);
        let point = vec3(at + 0x18);
        let on_blade = BLADE.lock().ok().and_then(|b| *b).map_or("blade unknown".to_owned(), |(start, unit, length)| {
            let d = [point[0] - start[0], point[1] - start[1], point[2] - start[2]];
            let out = d[0] * unit[0] + d[1] * unit[1] + d[2] * unit[2];
            let off = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2] - out * out).max(0.0).sqrt();
            format!("{out:.2} m out a {length:.2} m blade, {off:.2} m off it")
        });
        log!(
            "melee probe: {} ms hit {n} on {victim:#x} bone {} element {} point {point:.3?} ({on_blade}) direction {:.2?} side {:.2?}",
            ms(),
            mem::read::<i32>(at + 0x10).unwrap_or(-1),
            mem::read::<i32>(at + 0x14).unwrap_or(-1),
            vec3(at + 0x24),
            vec3(at + 0x30),
        );
    }
    // SAFETY: forwards the game's own call.
    unsafe { DEAL_ORIGINAL.get()(helper, result, info) }
}

pub fn report() {
    if INSTALLED.load(Relaxed) {
        log!("melee probe: {} damage messages, {} sweep frames, {} hits dealt", DAMAGES.load(Relaxed), SWEEPS.load(Relaxed), HITS.load(Relaxed));
    }
}
