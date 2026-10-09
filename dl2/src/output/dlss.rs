//! Per-eye DLSS: each eye gets its own feature (viewport id) and previous-frame matrices.

use crate::view::scene::TEMPORAL_EPOCH;
use crate::output::temporal::{DlssMatrices, PACKET_BYTES, ResetTracker};
use monaka_core::Aligned16;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

pub static EYE_CALLS: AtomicU64 = AtomicU64::new(0);
pub static RESETS: AtomicU64 = AtomicU64::new(0);
pub static MATRIX_FAILURES: AtomicU64 = AtomicU64::new(0);

struct State {
    matrices: DlssMatrices,
    resets: [ResetTracker; 2],
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State { matrices: DlssMatrices::default(), resets: Default::default() }));

/// A constants packet as the renderer reads it (aligned for SSE loads).
pub type Packet = Aligned16<[u8; PACKET_BYTES]>;

/// A DLSS call (constants or evaluate) made for eye `eye` (1 or 2; 0: the game's own): the
/// renderer's viewport id at `viewport` is swapped for the eye's own feature for the call (and a
/// cached copy at `cache`, when the renderer keeps one, put back after), and a constants call gets
/// a copy of `packet` with the eye's own history matrices. `original` is the renderer's own call,
/// given the packet to pass.
///
/// # Safety
/// `viewport` (and `cache`) point at the renderer's live u32 viewport ids, `packet` at a DLSS
/// constants packet of at least `PACKET_BYTES` bytes (for a constants call), and the caller holds
/// whatever lock keeps other DLSS calls out meanwhile.
pub unsafe fn for_eye(eye: u32, viewport: *mut u32, cache: Option<*mut u32>, packet: usize, evaluate: bool, original: impl FnOnce(usize) -> usize) -> usize {
    // SAFETY: the caller's guarantee.
    unsafe {
        let native = *viewport;
        if !(1..=2).contains(&eye) || native >= 0x4000_0000 {
            if eye != 0 {
                MATRIX_FAILURES.fetch_add(1, Ordering::Relaxed);
            }
            return original(packet);
        }
        let mapped = crate::output::temporal::eye_viewport(native, eye);
        let cached = cache.map(|c| *c);
        *viewport = mapped;
        let copy;
        let packet = if evaluate {
            EYE_CALLS.fetch_add(1, Ordering::Relaxed);
            packet
        } else {
            copy = eye_packet(packet, eye, mapped);
            copy.0.as_ptr() as usize
        };
        let result = original(packet);
        *viewport = native;
        if let (Some(cache), Some(cached)) = (cache, cached) {
            *cache = cached;
        }
        result
    }
}

/// A copy of the constants packet at `packet` with eye `eye`'s history matrices, and the reset flag
/// set when this eye's history starts over.
///
/// # Safety
/// `packet` must point at a DLSS constants packet of at least `PACKET_BYTES` bytes.
pub unsafe fn eye_packet(packet: usize, eye: u32, viewport: u32) -> Packet {
    let mut copy = Aligned16([0; PACKET_BYTES]);
    // SAFETY: the caller's guarantee.
    unsafe { std::ptr::copy_nonoverlapping(packet as *const u8, copy.0.as_mut_ptr(), PACKET_BYTES) };
    let flag = u32::from_le_bytes(copy.0[0x18..0x1c].try_into().expect("4 bytes"));
    let epoch = TEMPORAL_EPOCH.load(Ordering::Acquire);
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let mut reset = state.resets[(eye - 1) as usize].needs_reset(epoch, viewport) || flag != 0;
    if !state.matrices.apply(&mut copy.0, eye, epoch, viewport, reset) {
        MATRIX_FAILURES.fetch_add(1, Ordering::Relaxed);
        reset = true;
    }
    if reset {
        RESETS.fetch_add(1, Ordering::Relaxed);
        copy.0[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
    }
    copy
}
