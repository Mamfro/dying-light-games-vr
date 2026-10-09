//! `probe_history=1`: where the renderer keeps the previous frame's camera (read-only). DL2's
//! per-eye history hooked a tree lookup that The Beast no longer has, so it is found by value: with
//! a fixed head and the player standing still, each eye's camera position is constant, and the
//! previous-camera storage flips between the two every frame.
//!
//! 1. A background scan of the game's writable memory for either eye position, as a vector or as
//!    a row-major matrix's position column (components 16 bytes apart).
//! 2. Re-reads keep the places whose value alternates between the eyes.
//! 3. For a second of presents, each is classed against the eye just shown: holding it (the
//!    frame's own camera) or the other (the previous frame's, or the game's next update), and named
//!    by the C++ object it sits in.
//!
//! Beside it, the other probes of the renderer's camera history: `dump_object`, `vp_probe`,
//! `frame_probe` and `camera_refs` (each described at its statics below).

use super::options;
use monaka_hook::module::Module;
use monaka_hook::{AtomicF32, Hooks, Original, mem, probe};
use monaka_producer::{Rejection, log};
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU32};
use windows::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, PAGE_READWRITE, VirtualQuery};

/// The latest camera position written for each eye (world units).
static EYE_POSITION: [[AtomicF32; 3]; 2] = [const { [const { AtomicF32::zero() }; 3] }; 2];
static ARMED: AtomicBool = AtomicBool::new(false);
static PRESENTS_SAMPLED: AtomicU32 = AtomicU32::new(0);
static SAMPLES: Mutex<Vec<Candidate>> = Mutex::new(Vec::new());

/// Within this distance (world units, metres here) a stored position is taken as an eye's.
const NEAR: f32 = 0.005;
const MAX_FOUND: usize = 4096;
const MAX_KEPT: usize = 256;
const PRESENTS_TO_SAMPLE: u32 = 100;

struct Candidate {
    address: usize,
    /// Bytes between x, y and z: 4 for a vector, 16 for a row-major matrix's position column.
    stride: usize,
    shown: u32,
    other: u32,
    neither: u32,
}

/// The position at `address`, its components `stride` bytes apart.
fn read_position(address: usize, stride: usize) -> Option<[f32; 3]> {
    Some([mem::read::<f32>(address)?, mem::read::<f32>(address + stride)?, mem::read::<f32>(address + 2 * stride)?])
}

/// The eye camera just written (from the player camera update).
pub fn eye_written(eye: usize, position: [f32; 3]) {
    if !options().history_any() {
        return;
    }
    for (slot, value) in EYE_POSITION[eye.min(1)].iter().zip(position) {
        slot.store(value);
    }
}

fn eyes() -> [[f32; 3]; 2] {
    EYE_POSITION.each_ref().map(|p| [p[0].load(), p[1].load(), p[2].load()])
}

/// Which eye's position `v` is, if either.
fn which(v: [f32; 3], eyes: &[[f32; 3]; 2]) -> Option<usize> {
    (0..2).find(|&e| (0..3).all(|i| (v[i] - eyes[e][i]).abs() < NEAR))
}

/// The scans still due (`probe_history`, `vp_probe`; with stereo).
static SCAN_DUE: AtomicBool = AtomicBool::new(false);
static VP_SCAN_DUE: AtomicBool = AtomicBool::new(false);

/// The hooks are in (with `stereo`): the probes given an address arm, and the scans are due.
pub fn start(stereo: bool, renderer_base: usize) {
    let o = options();
    if stereo && let Some(spec) = &o.dump {
        arm_dump(spec);
    }
    let vp_probe = stereo && o.vp.as_deref().inspect(|spec| arm_vp(spec)).is_some();
    if stereo && o.frames {
        arm_frames(renderer_base);
    }
    SCAN_DUE.store(stereo && o.history, Release);
    VP_SCAN_DUE.store(vp_probe, Release);
}

/// The reporter thread, `elapsed` after it started: the history scan after three seconds, the
/// view-projection scan after two.
pub fn scans(elapsed: std::time::Duration) {
    if elapsed >= std::time::Duration::from_secs(3) && SCAN_DUE.swap(false, AcqRel) {
        scan();
    }
    if elapsed >= std::time::Duration::from_secs(2) && VP_SCAN_DUE.swap(false, AcqRel) {
        scan_vp();
    }
}

/// At a present that shows eye `shown` (render thread).
pub fn shown(shown: usize) {
    if !options().history_any() {
        return;
    }
    at_present(shown);
    dump_at_present(shown);
    vp_at_present();
    frames_at_present(shown);
}

/// The scan and re-reads (on the reporter thread, never a game thread).
fn scan() {
    let start = eyes();
    if start.iter().flatten().all(|v| *v == 0.0) || which(start[0], &[start[1], start[1]]).is_some() {
        log!("history probe: no distinct eye positions yet");
        return;
    }
    log!("history probe: scanning for eye positions {:?} / {:?}", start[0], start[1]);
    let began = std::time::Instant::now();
    let mut found = Vec::new();
    let mut scanned = 0usize;
    let mut address = 0x10000usize;
    let mut chunk = vec![0u8; 1 << 20];
    while address < 0x7fff_ffff_0000 && found.len() < MAX_FOUND {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: queries our own address space into a local.
        if unsafe { VirtualQuery(Some(address as *const _), &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
            break;
        }
        let (base, size) = (info.BaseAddress as usize, info.RegionSize);
        let writable = info.State == MEM_COMMIT && info.Protect == PAGE_READWRITE && (info.Protect & (PAGE_GUARD | PAGE_NOACCESS)).0 == 0;
        if writable && size <= 1 << 30 {
            let mut offset = 0;
            while offset < size && found.len() < MAX_FOUND {
                let length = chunk.len().min(size - offset);
                if mem::read_bytes(base + offset, &mut chunk[..length]) {
                    let eyes = eyes();
                    for i in (0..length.saturating_sub(12)).step_by(4) {
                        let x = f32::from_le_bytes(chunk[i..i + 4].try_into().expect("4 bytes"));
                        if (x - eyes[0][0]).abs() >= NEAR && (x - eyes[1][0]).abs() >= NEAR {
                            continue;
                        }
                        // Side by side (a vector), or 16 bytes apart (a matrix's position column).
                        for stride in [4, 16] {
                            if i + 2 * stride + 4 > length {
                                continue;
                            }
                            let at = |k: usize| f32::from_le_bytes(chunk[i + k * stride..i + k * stride + 4].try_into().expect("4 bytes"));
                            if which([at(0), at(1), at(2)], &eyes).is_some() {
                                found.push((base + offset + i, stride));
                            }
                        }
                    }
                }
                offset += length;
                scanned += length;
            }
        }
        address = base + size.max(0x1000);
    }
    log!("history probe: {} matches in {} MB in {:.1} s", found.len(), scanned >> 20, began.elapsed().as_secs_f64());
    // Re-read a few times, keeping the places that hold each eye at some point.
    let mut seen = vec![[false; 2]; found.len()];
    for _ in 0..12 {
        std::thread::sleep(std::time::Duration::from_millis(3));
        let eyes = eyes();
        for (&(address, stride), seen) in found.iter().zip(&mut seen) {
            if let Some(eye) = read_position(address, stride).and_then(|v| which(v, &eyes)) {
                seen[eye] = true;
            }
        }
    }
    let kept: Vec<Candidate> = found
        .iter()
        .zip(&seen)
        .filter(|(_, s)| s[0] && s[1])
        .take(MAX_KEPT)
        .map(|(&(address, stride), _)| Candidate { address, stride, shown: 0, other: 0, neither: 0 })
        .collect();
    log!("history probe: {} places alternate between the eyes; sampling them at {} presents", kept.len(), PRESENTS_TO_SAMPLE);
    *SAMPLES.lock().unwrap_or_else(|e| e.into_inner()) = kept;
    PRESENTS_SAMPLED.store(0, Relaxed);
    ARMED.store(true, Release);
}

/// At a present that shows eye `shown` (render thread): classes every kept place.
fn at_present(shown: usize) {
    if !ARMED.load(Acquire) {
        return;
    }
    let eyes = eyes();
    let mut samples = SAMPLES.lock().unwrap_or_else(|e| e.into_inner());
    for c in samples.iter_mut() {
        match read_position(c.address, c.stride).and_then(|v| which(v, &eyes)) {
            Some(e) if e == shown => c.shown += 1,
            Some(_) => c.other += 1,
            None => c.neither += 1,
        }
    }
    if PRESENTS_SAMPLED.fetch_add(1, Relaxed) + 1 >= PRESENTS_TO_SAMPLE {
        ARMED.store(false, Release);
        DONE.store(true, Release);
    }
}

static DONE: AtomicBool = AtomicBool::new(false);

/// Once the presents are sampled (reporter thread): each place with its counts and owner.
pub fn report_if_done() {
    if !DONE.swap(false, AcqRel) {
        return;
    }
    let samples = SAMPLES.lock().unwrap_or_else(|e| e.into_inner());
    let mut report: Vec<&Candidate> = samples.iter().collect();
    report.sort_by_key(|c| std::cmp::Reverse(c.shown.max(c.other)));
    log!("history probe: per place, presents holding the eye shown / the other eye / neither");
    for c in report.iter().take(80) {
        let kind = if c.stride == 4 { "vector" } else { "matrix" };
        log!("  {:#x} {kind}: {:3} / {:3} / {:3}  {}", c.address, c.shown, c.other, c.neither, owner(c.address));
    }
}

/// `dump_object=<address>:<length>[,<address>:<length>...]` (hex): each object's bytes at
/// consecutive presents, to see which fields follow the eyes and which hold another field's value
/// from the present before (a "previous" copy, whatever its format).
static DUMPS: Mutex<Vec<Dump>> = Mutex::new(Vec::new());
const DUMP_PRESENTS: usize = 6;
const DUMP_LINES: usize = 400;

struct Dump {
    address: usize,
    length: usize,
    frames: Vec<(usize, Vec<u8>)>,
    reported: bool,
}

/// Arms the dumps (`spec` as `address:length` pairs in hex, comma-separated).
fn arm_dump(spec: &str) {
    let parse = |s: &str| usize::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok();
    let mut dumps = DUMPS.lock().unwrap_or_else(|e| e.into_inner());
    for one in spec.split(',') {
        match one.split_once(':').and_then(|(a, l)| Some((parse(a)?, parse(l)?))) {
            Some((address, length)) if (8..=0x4000).contains(&length) => {
                log!("object dump armed: {address:#x}, {length:#x} bytes ({})", owner(address));
                dumps.push(Dump { address, length, frames: Vec::new(), reported: false });
            }
            _ => log!("dump_object {one}: expected <address>:<length> in hex, 8 to 0x4000 bytes"),
        }
    }
}

/// At a present showing eye `shown`: one copy of each object (one read each).
fn dump_at_present(shown: usize) {
    for d in DUMPS.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
        if d.frames.len() < DUMP_PRESENTS {
            let mut bytes = vec![0u8; d.length];
            if mem::read_bytes(d.address, &mut bytes) {
                d.frames.push((shown, bytes));
            }
        }
    }
}

/// Once the copies are in (reporter thread): every float that differs between them, by offset,
/// each eye's position components marked, and the fields that hold another field's last value.
pub fn report_dump() {
    let mut dumps = DUMPS.lock().unwrap_or_else(|e| e.into_inner());
    let eyes = eyes();
    for d in dumps.iter_mut().filter(|d| d.frames.len() == DUMP_PRESENTS && !d.reported) {
        d.reported = true;
        log!("object {:#x}: eyes shown {:?}; left eye at {:?}, right eye at {:?}", d.address, d.frames.iter().map(|f| f.0).collect::<Vec<_>>(), eyes[0], eyes[1]);
        let float = |bytes: &[u8], at: usize| f32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
        let changing: Vec<(usize, Vec<f32>)> = (0..d.length - 3)
            .step_by(4)
            .map(|at| (at, d.frames.iter().map(|(_, bytes)| float(bytes, at)).collect::<Vec<f32>>()))
            .filter(|(_, v)| !v.iter().all(|x| x.to_bits() == v[0].to_bits()))
            .collect();
        let mark = |v: f32| {
            let hits: Vec<String> = (0..2).flat_map(|e| (0..3).filter(move |&i| (v - eyes[e][i]).abs() < NEAR).map(move |i| format!("{}{}", ["L", "R"][e], ["x", "y", "z"][i]))).collect();
            hits.join("/")
        };
        for (at, values) in changing.iter().take(DUMP_LINES) {
            let shown: Vec<String> = values.iter().map(|&v| format!("{v:>12.5} {:5}", mark(v))).collect();
            log!("  +{at:#05x}: {}", shown.join(" "));
        }
        if changing.len() > DUMP_LINES {
            log!("  ... {} more changing floats", changing.len() - DUMP_LINES);
        }
        // A field whose every value is another field's value one present earlier.
        let close = |a: f32, b: f32| (a - b).abs() <= 1e-5 * a.abs().max(b.abs()).max(1.0);
        let mut lags = Vec::new();
        for (a, va) in &changing {
            for (b, vb) in &changing {
                if a != b && (1..DUMP_PRESENTS).all(|k| close(va[k], vb[k - 1])) && !(1..DUMP_PRESENTS).all(|k| close(va[k], vb[k])) {
                    lags.push(format!("+{a:#05x} <- +{b:#05x}"));
                }
            }
        }
        log!("  fields one present behind another (previous <- current): {}", if lags.is_empty() { "none".into() } else { lags.join(", ") });
    }
}

/// `vp_probe=<CShaderCamera address>` (hex): every copy of that camera's view-projection in memory
/// (matched on its first two rows' rotation part, so jitter and drift do not hide one), then, for a
/// few presents, whether each copy holds the camera's value from this present or the one before.
static VP: Mutex<Option<VpProbe>> = Mutex::new(None);
const VP_PRESENTS: usize = 8;
const VP_MAX: usize = 1024;
/// Jittered and unjittered view-projection (4x4, row-major) in `CShaderCamera`.
const SHADER_VIEWPROJ: usize = 0xc0;
const SHADER_VIEWPROJ_UNJITTERED: usize = 0x2c0;

type Mat = [f32; 16];

/// One present: the source's (jittered, unjittered) matrices and every copy's matrix.
type VpFrame = ([Mat; 2], Vec<Option<Mat>>);

struct VpProbe {
    source: usize,
    copies: Vec<usize>,
    frames: Vec<VpFrame>,
    armed: bool,
    reported: bool,
}

fn arm_vp(spec: &str) {
    match usize::from_str_radix(spec.trim().trim_start_matches("0x"), 16) {
        Ok(source) => {
            log!("view-projection probe armed on {source:#x} ({})", owner(source));
            *VP.lock().unwrap_or_else(|e| e.into_inner()) = Some(VpProbe { source, copies: Vec::new(), frames: Vec::new(), armed: false, reported: false });
        }
        Err(_) => log!("vp_probe={spec}: expected a hex address"),
    }
}

/// The scan (reporter thread): every place holding the source's first two rows' rotation part.
fn scan_vp() {
    let Some(source) = VP.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|p| p.source) else { return };
    let Some(key) = mem::read::<[f32; 16]>(source + SHADER_VIEWPROJ_UNJITTERED) else {
        log!("view-projection probe: source unreadable");
        return;
    };
    // Rotation parts of rows 0 and 1 (no translation, which drifts and differs between the eyes).
    let near = |a: f32, b: f32| (a - b).abs() < 0.01;
    let matches = |m: &[f32]| (0..3).all(|i| near(m[i], key[i]) && near(m[4 + i], key[4 + i]));
    let began = std::time::Instant::now();
    let mut copies = Vec::new();
    let mut address = 0x10000usize;
    let mut chunk = vec![0u8; 1 << 20];
    while address < 0x7fff_ffff_0000 && copies.len() < VP_MAX {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: queries our own address space into a local.
        if unsafe { VirtualQuery(Some(address as *const _), &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
            break;
        }
        let (base, size) = (info.BaseAddress as usize, info.RegionSize);
        if info.State == MEM_COMMIT && info.Protect == PAGE_READWRITE && size <= 1 << 30 {
            let mut offset = 0;
            while offset < size && copies.len() < VP_MAX {
                let length = chunk.len().min(size - offset);
                if mem::read_bytes(base + offset, &mut chunk[..length]) {
                    for i in (0..length.saturating_sub(32)).step_by(4) {
                        let x = f32::from_le_bytes(chunk[i..i + 4].try_into().expect("4 bytes"));
                        if !near(x, key[0]) {
                            continue;
                        }
                        let row: Vec<f32> = (0..8).map(|k| f32::from_le_bytes(chunk[i + 4 * k..i + 4 * k + 4].try_into().expect("4 bytes"))).collect();
                        if matches(&row) {
                            copies.push(base + offset + i);
                        }
                    }
                }
                offset += length;
            }
        }
        address = base + size.max(0x1000);
    }
    log!("view-projection probe: {} copies in {:.1} s; sampling {} presents", copies.len(), began.elapsed().as_secs_f64(), VP_PRESENTS);
    if let Some(p) = VP.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        p.copies = copies;
        p.armed = true;
    }
}

/// At a present (render thread): the source and every copy, once each.
fn vp_at_present() {
    let mut vp = VP.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = vp.as_mut().filter(|p| p.armed && p.frames.len() < VP_PRESENTS) else { return };
    let read = |at: usize| mem::read::<[f32; 16]>(at).unwrap_or([f32::NAN; 16]);
    let source = [read(p.source + SHADER_VIEWPROJ), read(p.source + SHADER_VIEWPROJ_UNJITTERED)];
    let copies = p.copies.iter().map(|&c| mem::read::<[f32; 16]>(c)).collect();
    p.frames.push((source, copies));
}

/// Once sampled (reporter thread): per copy, how often it held the source's jittered or unjittered
/// matrix of this present or of the one before.
pub fn report_vp() {
    let mut vp = VP.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = vp.as_mut().filter(|p| p.frames.len() == VP_PRESENTS && !p.reported) else { return };
    p.reported = true;
    let same = |a: &[f32; 16], b: &[f32; 16]| a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1e-4 * x.abs().max(y.abs()).max(1.0));
    log!("view-projection probe: per copy, presents matching the camera's [jittered now, unjittered now, jittered before, unjittered before] of {}", VP_PRESENTS - 1);
    let mut lines = Vec::new();
    for (index, &copy) in p.copies.iter().enumerate() {
        let mut counts = [0u32; 4];
        for k in 1..VP_PRESENTS {
            let Some(value) = p.frames[k].1[index] else { continue };
            let (now, before) = (&p.frames[k].0, &p.frames[k - 1].0);
            for (slot, reference) in [&now[0], &now[1], &before[0], &before[1]].into_iter().enumerate() {
                if same(&value, reference) {
                    counts[slot] += 1;
                }
            }
        }
        // Copies that only ever hold the current matrix are the camera's own; the rest matter.
        if counts[2] + counts[3] > 0 || counts.iter().all(|&c| c == 0) {
            lines.push((counts, copy));
        }
    }
    lines.sort_by_key(|(c, _)| std::cmp::Reverse(c[2] + c[3]));
    for (counts, copy) in lines.iter().take(60) {
        log!("  {copy:#x}: {counts:?}  {}", owner(*copy));
    }
    log!("  ({} copies hold only this present's matrix)", p.copies.len() - lines.len());
}

/// `frame_probe=1`: the renderer object's frame-data pointers (`CRenderer` at `renderer+0x32c8c0`;
/// its interface methods +0x210 and +0x218 return its +0x1888 and +0x1890) at consecutive
/// presents, each with the eye of the main view's camera it leads to (`views[20]` at +0xf0, a
/// `CShaderCamera` with its position at +0x4c/+0x5c/+0x6c).
static FRAMES: Mutex<Option<FrameProbe>> = Mutex::new(None);
const FRAME_PRESENTS: usize = 8;
const RENDERER_GLOBAL: usize = 0x32c8c0;
const FRAME_FIELDS: [usize; 6] = [0x1878, 0x1880, 0x1888, 0x1890, 0x1898, 0x18a0];

/// One present: the shown eye, and each field's pointer with its main view's eye.
type FrameRow = (usize, Vec<(usize, Option<usize>)>);

struct FrameProbe {
    renderer_base: usize,
    rows: Vec<FrameRow>,
    reported: bool,
}

fn arm_frames(renderer_base: usize) {
    log!("frame-data probe armed");
    *FRAMES.lock().unwrap_or_else(|e| e.into_inner()) = Some(FrameProbe { renderer_base, rows: Vec::new(), reported: false });
}

/// The main view camera's eye for a frame-data pointer.
fn frame_eye(frame: usize) -> Option<usize> {
    let camera = mem::read::<usize>(frame + 0xf0)?;
    which([mem::read(camera + 0x4c)?, mem::read(camera + 0x5c)?, mem::read(camera + 0x6c)?], &eyes())
}

fn frames_at_present(shown: usize) {
    let mut frames = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = frames.as_mut().filter(|p| p.rows.len() < FRAME_PRESENTS) else { return };
    let Some(renderer) = mem::read::<usize>(p.renderer_base + RENDERER_GLOBAL) else { return };
    let row = FRAME_FIELDS.iter().map(|&f| mem::read::<usize>(renderer + f).unwrap_or(0)).map(|v| (v, frame_eye(v))).collect();
    p.rows.push((shown, row));
}

pub fn report_frames() {
    let mut frames = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = frames.as_mut().filter(|p| p.rows.len() == FRAME_PRESENTS && !p.reported) else { return };
    p.reported = true;
    log!("frame-data probe: per present, the shown eye, then each field's pointer (and its main view's eye)");
    log!("  fields {:x?}", FRAME_FIELDS);
    for (shown, row) in &p.rows {
        let cells: Vec<String> = row.iter().map(|(v, e)| format!("{v:#x} ({})", e.map_or("-".into(), |e| ["L", "R"][e].to_string()))).collect();
        log!("  shown {}: {}", ["L", "R"][*shown], cells.join("  "));
    }
}

/// `camera_refs=1`: from inside the DLSS command's execute (render thread, frame data current),
/// every pointer to a camera in the frame render data and in the render context, labelled by the
/// eye of its position: this frame's, the other (the previous frame's), or neither.
static DLSS_EXECUTE: Original<crate::engine::CommandExecuteFn> = Original::new();
static REFS_LEFT: AtomicU32 = AtomicU32::new(0);
static REFS_RENDERER: AtomicUsize = AtomicUsize::new(0);
const REFS_FRAMES: u32 = 4;

/// Hooks the DLSS command's execute in `renderer` and arms the probe.
///
/// # Safety
/// `renderer` is the fingerprinted renderer.
pub unsafe fn install_refs(hooks: &mut Hooks, renderer: &Module) -> Result<(), Rejection> {
    let (rva, prologue) = crate::engine::DLSS_COMMAND_EXECUTE;
    // SAFETY: the DLSS command's execute in the fingerprinted renderer (prologue checked byte for
    // byte), of type CommandExecuteFn.
    unsafe { hooks.inline(&DLSS_EXECUTE, "CmdPPFX_DLSS execute", renderer.at(rva), prologue, dlss_execute as crate::engine::CommandExecuteFn)? };
    arm_refs(renderer.base());
    Ok(())
}

fn arm_refs(renderer_base: usize) {
    REFS_RENDERER.store(renderer_base, Relaxed);
    REFS_LEFT.store(REFS_FRAMES, Release);
    log!("camera reference probe armed ({REFS_FRAMES} frames)");
}

unsafe extern "system" fn dlss_execute(command: usize, context: usize) -> usize {
    let _flight = monaka_hook::InFlight::enter();
    if REFS_LEFT.load(Acquire) > 0 && REFS_LEFT.fetch_sub(1, AcqRel) > 0 {
        camera_refs(context);
    }
    // SAFETY: the original, with the renderer's arguments.
    unsafe { DLSS_EXECUTE.get()(command, context) }
}

/// The eye a camera object's position is (`None` if not a camera or neither eye).
fn camera_eye(object: usize, eyes: &[[f32; 3]; 2]) -> Option<(String, Option<usize>)> {
    let class = probe::class_name(object)?;
    if !class.contains("Camera") {
        return None;
    }
    let position = [mem::read(object + 0x4c)?, mem::read(object + 0x5c)?, mem::read(object + 0x6c)?];
    Some((class, which(position, eyes)))
}

fn camera_refs(context: usize) {
    let eyes = eyes();
    let rendering = crate::view::stereo::rendering_eye();
    let renderer = mem::read::<usize>(REFS_RENDERER.load(Relaxed) + crate::engine::RENDERER_GLOBAL).unwrap_or(0);
    let frame = mem::read::<usize>(renderer + crate::engine::RENDERER_FRAME_DATA).unwrap_or(0);
    let main_view = mem::read::<usize>(frame + 0xf0).and_then(|c| camera_eye(c, &eyes));
    log!("camera refs: rendering eye {rendering:?}, frame data {frame:#x}, main view {main_view:?}, context {context:#x}");
    let label = |eye: Option<usize>| match (eye, main_view.as_ref().and_then(|m| m.1)) {
        (Some(e), Some(m)) if e == m => "THIS frame",
        (Some(_), Some(_)) => "OTHER eye",
        (Some(_), None) => "an eye",
        (None, _) => "neither",
    };
    for (name, base, length) in [("frame data", frame, 0x1000usize), ("context", context, 0x2400usize)] {
        if base == 0 {
            continue;
        }
        // In 256-byte pieces: the frame data lives on the render thread's stack, and a read past its
        // end must not lose the rest.
        let mut block = vec![0u8; length];
        let readable: Vec<bool> = block.chunks_mut(0x100).enumerate().map(|(i, piece)| mem::read_bytes(base + i * 0x100, piece)).collect();
        log!("  {name}: {} of {} pieces readable", readable.iter().filter(|r| **r).count(), readable.len());
        for at in (0..length).step_by(8) {
            if !readable[at / 0x100] {
                continue;
            }
            let pointer = usize::from_le_bytes(block[at..at + 8].try_into().expect("8 bytes"));
            if !(0x10000..0x7fff_ffff_0000).contains(&pointer) || pointer & 7 != 0 {
                continue;
            }
            if let Some((class, eye)) = camera_eye(pointer, &eyes) {
                log!("  {name} +{at:#06x} -> {pointer:#x} {class}: {}", label(eye));
            }
        }
    }
}

/// The C++ object a place sits in: the nearest vtable pointer before it with an RTTI name.
fn owner(address: usize) -> String {
    for back in (0..0x4000usize).step_by(8) {
        let Some(start) = (address & !7).checked_sub(back) else { break };
        if let Some(name) = probe::class_name(start).filter(|n| n.starts_with(".?AV") || n.starts_with(".?AU")) {
            let vtable = mem::read::<usize>(start).unwrap_or(0);
            return format!("in {name} at +{:#x} (object {start:#x}, vtable {})", address - start, Module::describe(vtable));
        }
    }
    "(no object found)".into()
}
