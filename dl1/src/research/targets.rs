//! `probe_targets`: every render target the game draws into on the immediate context, in frame
//! order, and the sets of targets bound together with a depth buffer. Run once with `MotionBlur(0)`
//! and once with `MotionBlur(1)` in `video.scr`: a velocity buffer is a two-channel target (R16G16,
//! R8G8) that only the second run draws into. Drawn into alongside the G-buffer with depth means
//! per-object motion; drawn by a few full-screen draws means camera motion only, which we can
//! compute ourselves from depth and the cameras we set.
//!
//! The G-buffer is four targets bound with depth (RGBA8, RGBA8, R10G10B10A2, R16G16_FLOAT), and
//! the R16G16_FLOAT one is written with motion blur on or off; with blur on, a chain of
//! R16G16_FLOAT targets halving in size follows (the blur's tile maximum). The probe also dumps that fourth target a few times
//! ([`scene_copied`]) to see what it holds.

use monaka_channel::d3d;
use monaka_core::{bmp, half};
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::*};
use windows::Win32::Graphics::Direct3D11::{ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D};
use windows::core::Interface;

static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn enable() {
    ENABLED.store(true, Release);
}

pub fn enabled() -> bool {
    ENABLED.load(Relaxed)
}

#[derive(Clone, Copy)]
struct Target {
    texture: usize,
    width: u32,
    height: u32,
    format: i32,
    /// Slots it was bound to (bit per slot).
    slots: u8,
    draws: u64,
    /// Draws of at most four vertices (full-screen passes).
    fullscreen_draws: u64,
    /// Draws with a depth buffer bound beside it.
    depth_draws: u64,
    frames: u64,
    last_frame: u64,
    /// Sum of its first-draw position in each frame, for the average order.
    order_sum: u64,
}

struct Set {
    textures: [usize; 8],
    with_depth: bool,
    draws: u64,
}

struct Probe {
    targets: Vec<Target>,
    sets: Vec<Set>,
    /// What is bound now: the textures by slot and whether a depth buffer is.
    bound: [usize; 8],
    bound_depth: bool,
    frame: u64,
    /// Distinct targets first drawn this frame so far.
    order: u64,
}

static PROBE: Mutex<Probe> = Mutex::new(Probe { targets: Vec::new(), sets: Vec::new(), bound: [0; 8], bound_depth: false, frame: 1, order: 0 });

/// The game bound `count` render targets (and a depth buffer or not).
pub fn bound(count: u32, views: *const *mut core::ffi::c_void, depth: bool) {
    if !enabled() {
        return;
    }
    let Ok(mut probe) = PROBE.lock() else { return };
    probe.bound = [0; 8];
    probe.bound_depth = depth;
    if views.is_null() {
        return;
    }
    for slot in 0..count.min(8) as usize {
        // SAFETY: the game passes an array of `count` views (each live or null).
        let raw = unsafe { *views.add(slot) };
        // SAFETY: a live render target view or null.
        let Some(view) = (unsafe { ID3D11RenderTargetView::from_raw_borrowed(&raw) }) else { continue };
        // SAFETY: COM call on a live view.
        let Some(texture) = (unsafe { view.GetResource() }).ok().and_then(|r| r.cast::<ID3D11Texture2D>().ok()) else { continue };
        let key = texture.as_raw() as usize;
        probe.bound[slot] = key;
        if let Some(known) = probe.targets.iter_mut().find(|t| t.texture == key) {
            known.slots |= 1 << slot;
        } else if probe.targets.len() < 256 {
            let desc = d3d::texture_desc(&texture);
            probe.targets.push(Target {
                texture: key,
                width: desc.Width,
                height: desc.Height,
                format: desc.Format.0,
                slots: 1 << slot,
                draws: 0,
                fullscreen_draws: 0,
                depth_draws: 0,
                frames: 0,
                last_frame: 0,
                order_sum: 0,
            });
        }
    }
    drop(probe);
    if let Some(motion) = crate::output::upscale::motion_target(count, views, depth) {
        let mut gbuffer = GBUFFER.lock().unwrap_or_else(|e| e.into_inner());
        if gbuffer.0.as_ref().is_none_or(|known| known.as_raw() != motion.as_raw()) {
            gbuffer.0 = Some(motion);
        }
    }
}

/// A draw of `vertices` vertices into whatever is bound.
pub fn draw(vertices: u32) {
    if !enabled() {
        return;
    }
    let Ok(mut probe) = PROBE.lock() else { return };
    let Probe { targets, sets, bound, bound_depth, frame, order } = &mut *probe;
    if bound.iter().all(|&t| t == 0) {
        return;
    }
    for &key in bound.iter().filter(|&&t| t != 0) {
        let Some(target) = targets.iter_mut().find(|t| t.texture == key) else { continue };
        target.draws += 1;
        if vertices <= 4 {
            target.fullscreen_draws += 1;
        }
        if *bound_depth {
            target.depth_draws += 1;
        }
        if target.last_frame != *frame {
            target.last_frame = *frame;
            target.frames += 1;
            target.order_sum += *order;
            *order += 1;
        }
    }
    if let Some(set) = sets.iter_mut().find(|s| s.textures == *bound && s.with_depth == *bound_depth) {
        set.draws += 1;
    } else if sets.len() < 128 {
        sets.push(Set { textures: *bound, with_depth: *bound_depth, draws: 1 });
    }
}

/// A present: the next frame starts.
pub fn frame_end() {
    if !enabled() {
        return;
    }
    let Ok(mut probe) = PROBE.lock() else { return };
    probe.frame += 1;
    probe.order = 0;
}

fn format_name(format: i32) -> &'static str {
    match format {
        2 => "R32G32B32A32_FLOAT",
        10 => "R16G16B16A16_FLOAT",
        11 => "R16G16B16A16_UNORM",
        24 => "R10G10B10A2_UNORM",
        26 => "R11G11B10_FLOAT",
        27..=29 => "R8G8B8A8",
        33 => "R16G16_TYPELESS (2ch)",
        34 => "R16G16_FLOAT (2ch)",
        35 => "R16G16_UNORM (2ch)",
        37 => "R16G16_SNORM (2ch)",
        41 => "R32_FLOAT",
        48 => "R8G8_TYPELESS (2ch)",
        49 => "R8G8_UNORM (2ch)",
        51 => "R8G8_SNORM (2ch)",
        54 => "R16_FLOAT",
        56 => "R16_UNORM",
        61 => "R8_UNORM",
        87 | 88 | 90 | 91 => "B8G8R8A8/X8",
        _ => "",
    }
}

/// Logs the targets in average frame order, then the target sets drawn with depth (end of a run).
pub fn report() {
    if !enabled() {
        return;
    }
    let Ok(mut probe) = PROBE.lock() else { return };
    let frames = probe.frame.saturating_sub(1).max(1);
    log!("probe_targets: {} targets over {frames} frames", probe.targets.len());
    probe.targets.retain(|t| t.draws > 0);
    probe.targets.sort_by(|a, b| {
        let order = |t: &Target| t.order_sum as f64 / t.frames.max(1) as f64;
        order(a).total_cmp(&order(b))
    });
    for t in &probe.targets {
        log!(
            "target {:#x}: {}x{} format {} {} slots={:#04x} frames={} order={:.1} draws/frame={:.1} fullscreen/frame={:.1} with_depth/frame={:.1}",
            t.texture,
            t.width,
            t.height,
            t.format,
            format_name(t.format),
            t.slots,
            t.frames,
            t.order_sum as f64 / t.frames.max(1) as f64,
            t.draws as f64 / frames as f64,
            t.fullscreen_draws as f64 / frames as f64,
            t.depth_draws as f64 / frames as f64,
        );
    }
    probe.sets.sort_by(|a, b| b.draws.cmp(&a.draws));
    for s in probe.sets.iter().filter(|s| s.with_depth && s.textures.iter().filter(|&&t| t != 0).count() > 1) {
        let members: Vec<String> = s.textures.iter().filter(|&&t| t != 0).map(|t| format!("{t:#x}")).collect();
        log!("set with depth: [{}] draws/frame={:.1}", members.join(", "), s.draws as f64 / frames as f64);
    }
}

/// The G-buffer's fourth target (R16G16_FLOAT), for the dumps.
struct Held(Option<ID3D11Texture2D>);
// SAFETY: used under the mutex on the game's render thread, and released after the hooks are off.
unsafe impl Send for Held {}
static GBUFFER: Mutex<Held> = Mutex::new(Held(None));

/// Frames whose fourth G-buffer target is dumped: a few, about half a second apart.
const DUMP_FROM: u64 = 200;
const DUMP_EVERY: u64 = 50;
const DUMPS: u64 = 8;

/// The frame's 3D image just reached the back buffer, so the G-buffer is complete: on a dump
/// frame, copy the fourth target to the CPU, log its statistics and write it beside the log
/// (`velocity-<frame>-<w>x<h>.rg16f`, raw half floats, and a `.bmp` with x in red and y in green).
pub fn scene_copied(context: &ID3D11DeviceContext) {
    if !enabled() {
        return;
    }
    let frame = PROBE.lock().map(|p| p.frame).unwrap_or(0);
    if frame < DUMP_FROM || !(frame - DUMP_FROM).is_multiple_of(DUMP_EVERY) || (frame - DUMP_FROM) / DUMP_EVERY >= DUMPS {
        return;
    }
    let Some(texture) = GBUFFER.lock().ok().and_then(|g| g.0.clone()) else { return log!("dump: no R16G16_FLOAT G-buffer target seen") };
    // A staging copy on the game's device and immediate context, on its render thread (stalls;
    // probing only).
    match d3d::texture_device(&texture).and_then(|device| d3d::read_texels(&device, context, &texture)) {
        Ok((width, height, 4, texels)) => dump(frame, width, height, &texels),
        Ok((_, _, texel, _)) => log!("dump at frame {frame}: {texel} bytes per texel, not R16G16"),
        Err(why) => log!("dump at frame {frame}: {why}"),
    }
}

/// Mean of the finite values inside a region.
fn region_mean(values: &[[f32; 2]], width: u32, inside: impl Fn(u32, u32) -> bool) -> [f32; 2] {
    let (mut sum, mut n) = ([0.0f32; 2], 0.0f32);
    for (i, v) in values.iter().enumerate() {
        let (x, y) = (i as u32 % width, i as u32 / width);
        if inside(x, y) && v[0].is_finite() && v[1].is_finite() {
            sum = [sum[0] + v[0], sum[1] + v[1]];
            n += 1.0;
        }
    }
    [sum[0] / n.max(1.0), sum[1] / n.max(1.0)]
}

/// `texels`: the target's rows, tightly packed, two half floats per texel.
fn dump(frame: u64, width: u32, height: u32, texels: &[u8]) {
    let half_at = |bytes: &[u8]| half::to_f32(u16::from_le_bytes([bytes[0], bytes[1]]));
    let values: Vec<[f32; 2]> = texels.chunks_exact(4).map(|t| [half_at(&t[..2]), half_at(&t[2..])]).collect();
    let finite = || values.iter().filter(|v| v[0].is_finite() && v[1].is_finite());
    let count = finite().count().max(1) as f32;
    let zero = finite().filter(|v| v[0] == 0.0 && v[1] == 0.0).count() as f32 / count;
    let mean = |c: usize| finite().map(|v| v[c]).sum::<f32>() / count;
    let mean_abs = |c: usize| finite().map(|v| v[c].abs()).sum::<f32>() / count;
    let range = |c: usize| finite().fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v[c]), hi.max(v[c])));
    // Each quarter's mean (left, right, top, bottom): a turn moves them alike, a step forward
    // moves them apart.
    let (w4, h4) = (width / 4, height / 4);
    let left = region_mean(&values, width, |x, _| x < w4);
    let right = region_mean(&values, width, |x, _| x >= width - w4);
    let top = region_mean(&values, width, |_, y| y < h4);
    let bottom = region_mean(&values, width, |_, y| y >= height - h4);
    let centre = values[(height / 2 * width + width / 2) as usize];
    log!(
        "dump frame {frame}: {width}x{height} zero={zero:.3} x mean={:.5} |x|={:.5} range={:?} y mean={:.5} |y|={:.5} range={:?} centre={centre:?} left={left:?} right={right:?} top={top:?} bottom={bottom:?} nonfinite={}",
        mean(0),
        mean_abs(0),
        range(0),
        mean(1),
        mean_abs(1),
        range(1),
        values.len() - finite().count(),
    );
    let Some(session) = monaka_producer::Session::get() else { return };
    if let Err(e) = std::fs::write(session.path(&format!("velocity-{frame}-{width}x{height}.rg16f")), texels) {
        log!("dump at frame {frame}: {e}");
    }
    // Half size, x in red and y in green around grey, scaled so three mean magnitudes fill the range.
    let scale = 127.0 / (3.0 * mean_abs(0).max(mean_abs(1)).max(1e-6));
    let byte = |v: f32| if v.is_finite() { (128.0 + v * scale).clamp(0.0, 255.0) as u8 } else { 255 };
    let image = bmp::rgb(width / 2, height / 2, |col, row| {
        let v = values[((row * 2) * width + col * 2) as usize];
        [byte(v[0]), byte(v[1]), 128]
    });
    if let Err(e) = std::fs::write(session.path(&format!("velocity-{frame}.bmp")), image) {
        log!("dump at frame {frame}: {e}");
    }
}

/// Lets go of the G-buffer target (after the hooks are off).
pub fn release() {
    GBUFFER.lock().unwrap_or_else(|e| e.into_inner()).0 = None;
}
