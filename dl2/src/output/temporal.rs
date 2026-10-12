//! Per-eye temporal history. Each eye keeps its own previous camera and its own DLSS history, so
//! the left eye is reprojected against the previous left eye rather than the right. Adapted from
//! farmerarmor/DyingLight2VR TemporalHistory.h and DlssHistory.h (MIT).

use crate::engine::CAMERA_WRITER_RETURNS;
use monaka_core::Aligned16;
use monaka_core::eye_history::{EyeHistory, Rules};
use monaka_core::mat4d::{Mat4d, inverse, mul};

/// Which of the scene's seven camera writers a return address (RVA) is, as a bit.
pub fn camera_writer_bit(return_rva: usize) -> u32 {
    CAMERA_WRITER_RETURNS.iter().position(|&r| r == return_rva).map_or(0, |i| 1 << i)
}
pub const ALL_CAMERA_WRITERS: u32 = 127;

/// Saved state per (owner, key) and eye. A new epoch (mono frame, skipped frame, reset) drops both
/// eyes' copies; the next use seeds each from the native state.
pub struct HistoryBank<const BYTES: usize> {
    entries: Vec<Entry<BYTES>>,
}

struct Entry<const BYTES: usize> {
    owner: usize,
    key: u32,
    epoch: u32,
    valid: [bool; 2],
    data: [Block<BYTES>; 2],
}

/// The engine may read a saved camera with aligned SSE loads.
type Block<const BYTES: usize> = Aligned16<[u8; BYTES]>;

impl<const BYTES: usize> HistoryBank<BYTES> {
    pub fn new(capacity: usize) -> Self {
        Self { entries: (0..capacity).map(|_| Entry { owner: 0, key: 0, epoch: 0, valid: [false; 2], data: [Aligned16([0; BYTES]), Aligned16([0; BYTES])] }).collect() }
    }

    /// This eye's copy for (owner, key), seeded from `seed` (BYTES readable bytes) when new.
    ///
    /// # Safety
    /// `seed` must point at `BYTES` readable bytes.
    pub unsafe fn get(&mut self, owner: usize, key: u32, epoch: u32, eye: u32, seed: *const u8) -> Option<*mut u8> {
        if owner == 0 || seed.is_null() || !(1..=2).contains(&eye) {
            return None;
        }
        let index = match self.entries.iter().position(|e| e.owner == owner && e.key == key) {
            Some(i) => i,
            None => {
                let i = self.entries.iter().position(|e| e.owner == 0 || e.epoch != epoch)?;
                let e = &mut self.entries[i];
                (e.owner, e.key, e.epoch, e.valid) = (owner, key, epoch, [false; 2]);
                i
            }
        };
        let entry = &mut self.entries[index];
        if entry.epoch != epoch {
            entry.epoch = epoch;
            entry.valid = [false; 2];
        }
        let i = (eye - 1) as usize;
        if !entry.valid[i] {
            // SAFETY: `seed` holds BYTES readable bytes (the caller's guarantee).
            unsafe { std::ptr::copy_nonoverlapping(seed, entry.data[i].0.as_mut_ptr(), BYTES) };
            entry.valid[i] = true;
        }
        Some(entry.data[i].0.as_mut_ptr())
    }

    /// Stores `current` as this eye's copy.
    ///
    /// # Safety
    /// `current` must point at `BYTES` readable bytes.
    pub unsafe fn store(&mut self, owner: usize, key: u32, epoch: u32, eye: u32, current: *const u8) -> bool {
        // SAFETY: as for `get`.
        match unsafe { self.get(owner, key, epoch, eye, current) } {
            Some(dest) => {
                // SAFETY: `dest` is this entry's BYTES-sized buffer; `current` is readable.
                unsafe { std::ptr::copy(current, dest, BYTES) };
                true
            }
            None => false,
        }
    }
}

/// DLSS keeps one feature (and history) per viewport id; the eyes get their own ids
/// ([`monaka_streamline::eye_viewport`]). An id already using the top bits is left as it is.
pub fn eye_viewport(original: u32, eye: u32) -> u32 {
    if !(1..=2).contains(&eye) || original >= 0x4000_0000 { original } else { monaka_streamline::eye_viewport(original, eye) }
}

type Matrix = Mat4d;

fn read_f32(p: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(p[offset..offset + 4].try_into().expect("4 bytes"))
}

#[allow(clippy::needless_range_loop)]
/// The DLSS constants packet's clip transform: projection (column-major) at +0x30, camera position
/// at +0x170, right/up/view-z rows at +0x188/+0x17c/+0x194.
fn clip(packet: &[u8]) -> Option<Matrix> {
    let mut p = [0.0; 16];
    let mut v = [0.0; 16];
    v[15] = 1.0;
    for i in 0..4 {
        for j in 0..4 {
            p[i * 4 + j] = read_f32(packet, 0x30 + 4 * (j * 4 + i)) as f64;
            if !p[i * 4 + j].is_finite() {
                return None;
            }
        }
    }
    const BASIS: [usize; 3] = [0x188, 0x17c, 0x194];
    for (i, &row) in BASIS.iter().enumerate() {
        for j in 0..3 {
            let (x, pos) = (read_f32(packet, row + 4 * j) as f64, read_f32(packet, 0x170 + 4 * j) as f64);
            if !x.is_finite() || !pos.is_finite() {
                return None;
            }
            v[i * 4 + j] = x;
            v[i * 4 + 3] -= x * pos;
        }
    }
    Some(mul(&p, &v))
}

#[allow(clippy::needless_range_loop)]
fn write(packet: &mut [u8], offset: usize, m: &Matrix) {
    for i in 0..4 {
        for j in 0..4 {
            let at = offset + 4 * (j * 4 + i);
            packet[at..at + 4].copy_from_slice(&(m[i * 4 + j] as f32).to_le_bytes());
        }
    }
}

/// Size of the DLSS constants packet this code touches.
pub const PACKET_BYTES: usize = 0x1b0;

/// What an eye's DLSS history carries on through: the temporal epoch (a mono or skipped frame
/// starts a new one), the eye's viewport id and the render size.
type Shape = (u32, u32, u32, u32);

/// The DLSS constants packet carries clip-to-previous-clip matrices built from the native camera
/// history; these are rebuilt from each eye's own previous clip transform, kept per eye
/// (`monaka_core::eye_history`).
pub struct DlssMatrices {
    history: EyeHistory<Shape, Matrix>,
}

impl Default for DlssMatrices {
    fn default() -> Self {
        Self { history: EyeHistory::new(Rules::OWN) }
    }
}

impl DlssMatrices {
    /// Writes this eye's (1 or 2) clip-to-previous (+0xf0) and previous-to-clip (+0x130), and sets
    /// the reset flag (+0x18) when its history starts over: its first frame, `reset` asked for, a
    /// new epoch, viewport or size. Whether it started over; `None` when the matrices could not be
    /// made.
    pub fn apply(&mut self, packet: &mut [u8; PACKET_BYTES], eye: u32, epoch: u32, viewport: u32, reset: bool) -> Option<bool> {
        if !(1..=2).contains(&eye) {
            return None;
        }
        let current = clip(packet)?;
        let inverse_current = inverse(&current)?;
        let width = u32::from_le_bytes(packet[0x24..0x28].try_into().expect("4 bytes"));
        let height = u32::from_le_bytes(packet[0x28..0x2c].try_into().expect("4 bytes"));
        let eye = (eye - 1) as usize;
        self.history.frame(eye, (epoch, viewport, width, height), reset);
        let previous = self.history.previous(eye).copied();
        let seed = previous.is_none();
        let to_previous = mul(&previous.unwrap_or(current), &inverse_current);
        let to_current = inverse(&to_previous)?;
        write(packet, 0xf0, &to_previous);
        write(packet, 0x130, &to_current);
        if seed {
            packet[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
        }
        self.history.keep(eye, current);
        Some(seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_bits_cover_all_seven() {
        let all = CAMERA_WRITER_RETURNS.iter().fold(0, |m, &r| m | camera_writer_bit(r));
        assert_eq!(all, ALL_CAMERA_WRITERS);
        assert_eq!(camera_writer_bit(0x1234), 0);
    }

    #[test]
    fn eye_viewports_differ() {
        assert_eq!(eye_viewport(5, 1), 5 | (1 << 30));
        assert_eq!(eye_viewport(5, 0), 5);
        assert_eq!(eye_viewport(0x4000_0001, 1), 0x4000_0001);
    }

    #[test]
    fn history_seeds_then_keeps_each_eye() {
        let mut bank = HistoryBank::<4>::new(2);
        let seed = [1u8, 2, 3, 4];
        let left = unsafe { bank.get(10, 7, 1, 1, seed.as_ptr()) }.unwrap();
        assert_eq!(left as usize % 16, 0, "saved state is 16-byte aligned");
        unsafe { *left = 9 };
        let again = unsafe { bank.get(10, 7, 1, 1, [0u8; 4].as_ptr()) }.unwrap();
        assert_eq!(unsafe { *again }, 9);
        // A new epoch reseeds.
        let reseeded = unsafe { bank.get(10, 7, 2, 1, seed.as_ptr()) }.unwrap();
        assert_eq!(unsafe { *reseeded }, 1);
    }

    #[test]
    fn identity_motion_gives_identity_matrices() {
        let mut packet = [0u8; PACKET_BYTES];
        for (i, v) in [1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, -1.0, 0.0, 0.0, 0.1, 0.0].iter().enumerate() {
            packet[0x30 + 4 * i..0x34 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        for (row, v) in [(0x188, [1.0f32, 0.0, 0.0]), (0x17c, [0.0, 1.0, 0.0]), (0x194, [0.0, 0.0, 1.0])] {
            for (j, x) in v.iter().enumerate() {
                packet[row + 4 * j..row + 4 * j + 4].copy_from_slice(&x.to_le_bytes());
            }
        }
        let mut m = DlssMatrices::default();
        assert_eq!(m.apply(&mut packet, 1, 1, 3, false), Some(true));
        assert_eq!(u32::from_le_bytes(packet[0x18..0x1c].try_into().unwrap()), 1, "first use seeds");
        packet[0x18..0x1c].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(m.apply(&mut packet, 1, 1, 3, false), Some(false));
        assert_eq!(u32::from_le_bytes(packet[0x18..0x1c].try_into().unwrap()), 0, "same epoch keeps history");
        assert!((read_f32(&packet, 0xf0) - 1.0).abs() < 1e-5);
        assert_eq!(m.apply(&mut packet, 2, 1, 3, false), Some(true), "the other eye seeds its own");
        assert_eq!(m.apply(&mut packet, 1, 2, 3, false), Some(true), "a new epoch seeds again");
        assert_eq!(m.apply(&mut packet, 1, 2, 4, false), Some(true), "and a new viewport");
        assert_eq!(m.apply(&mut packet, 1, 2, 4, true), Some(true), "and a reset asked for");
        assert_eq!(m.apply(&mut packet, 1, 2, 4, false), Some(false));
        assert_eq!(m.apply(&mut packet, 3, 2, 4, false), None, "eyes are 1 and 2");
    }
}
