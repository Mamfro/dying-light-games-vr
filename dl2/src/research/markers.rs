//! The HUD world markers' probe (`crate::hud::markers`).
//!
//! `probe_markers`: logs the HUD's projector calls (world point, the game's result, the camera it
//! projected with) against the frame's camera from the DLSS constants, and dumps the UI layer
//! twice (`ui-<present>.bmp` colour over grey, `ui-<present>-alpha.bmp`) to measure where and how
//! big markers are drawn.

use super::options;
use crate::hud::markers::{Call, Marker};
use monaka_core::bmp;
use monaka_hook::mem;
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::*};
use windows::Win32::Graphics::Direct3D12::*;

static LATEST_LOGGED: AtomicU64 = AtomicU64::new(0);
static LOGGED: AtomicU64 = AtomicU64::new(0);

/// The frame's camera from the DLSS constants: world-to-clip (row vectors) and position.
#[derive(Clone, Copy)]
struct FrameCamera {
    view_projection: [f32; 16],
    position: [f32; 3],
    forward: [f32; 3],
    near: f32,
}

static CAMERA: Mutex<Option<FrameCamera>> = Mutex::new(None);

/// The DLSS constants the game set: the frame's camera (depth stereo: the centre camera).
pub fn constants(constants: &monaka_streamline::Constants) {
    if !options().markers {
        return;
    }
    use monaka_streamline::constants as at;
    let Some(view_projection) = constants.view_projection() else { return };
    let camera = FrameCamera { view_projection, position: constants.vec3(at::POSITION), forward: constants.vec3(at::FORWARD), near: constants.f32(at::NEAR) };
    if let Ok(mut slot) = CAMERA.lock() {
        *slot = Some(camera);
    }
}

/// The markers the HUD placed in its latest tick, as found at present `present` with the frame's
/// camera at `position`.
pub fn latest(present: u64, position: [f32; 3], markers: &[Marker]) {
    if options().markers && !markers.is_empty() && LATEST_LOGGED.fetch_add(1, Relaxed) < 20 {
        log!("markers at present {present}: camera at {:?}, {:?}", position, markers);
    }
}

/// One HUD projector call, with the camera the HUD passed and its `option` argument.
pub fn call(call: &Call, camera: usize, option: bool) {
    if !options().markers || LOGGED.fetch_add(1, Relaxed) >= 120 {
        return;
    }
    // The camera the HUD projected with (as DL2VR reads it: its native camera at +0x38, that
    // camera's camera-to-world at +0x40 and projection at +0x80).
    let native = mem::read::<usize>(camera + 0x38).unwrap_or(0);
    let matrix = mem::read::<[f32; 12]>(native + 0x40);
    let projection = mem::read::<[f32; 16]>(native + 0x80);
    let ours = project_frame(&call.world);
    log!(
        "marker call present={} caller={:#x} world={:?} out={:?} clipped={} clamp={} option={option} frame_camera_pixel_ndc={ours:?} gui_camera={matrix:?} gui_projection={projection:?}",
        call.present,
        call.caller,
        call.world,
        call.out,
        call.clipped,
        call.clamp
    );
}

/// `world` through the frame's camera: normalised device x, y (y up) and its reverse-Z depth.
fn project_frame(world: &[f32; 3]) -> Option<[f32; 3]> {
    let camera = (*CAMERA.lock().ok()?)?;
    let m = &camera.view_projection;
    let v = [world[0], world[1], world[2], 1.0];
    let clip: [f32; 4] = std::array::from_fn(|c| (0..4).map(|r| v[r] * m[r * 4 + c]).sum());
    let along = (0..3).map(|i| (world[i] - camera.position[i]) * camera.forward[i]).sum::<f32>();
    (clip[3].abs() > 1e-6).then(|| [clip[0] / clip[3], clip[1] / clip[3], camera.near / along.max(1e-3)])
}

static DUMPS: AtomicU64 = AtomicU64::new(0);
const DUMP_AT: [u64; 2] = [250, 500];

/// The UI layer `ui` (in COMMON) on `queue` after a depth-stereo present: dumped at a couple of
/// presents, with the recent HUD marker calls projected through the frame's camera.
pub fn ui(queue: &ID3D12CommandQueue, ui: &ID3D12Resource) {
    if !options().markers {
        return;
    }
    let present = crate::PRESENTS.load(Relaxed);
    let done = DUMPS.load(Relaxed) as usize;
    if done >= DUMP_AT.len() || present < DUMP_AT[done] {
        return;
    }
    DUMPS.fetch_add(1, Relaxed);
    let calls = crate::hud::markers::recent();
    match monaka_channel::d3d12::read_texture(queue, ui, D3D12_RESOURCE_STATE_COMMON) {
        Ok((w, _, row_bytes, _)) if row_bytes != w as usize * 4 => log!("UI dump: {row_bytes} bytes per row of {w}, not 8-bit RGBA"),
        Ok((w, h, _, pixels)) => {
            log!("UI dump at present {present}: {w}x{h}, {} HUD marker calls in the last frames", calls.len());
            for call in &calls {
                let ours = project_frame(&call.world).map(|[x, y, d]| [(x + 1.0) * 0.5 * w as f32, (1.0 - y) * 0.5 * h as f32, d]);
                log!(
                    "  present={} caller={:#x} world={:?} out={:?} clipped={} clamp={} frame_camera_pixel_and_depth={ours:?}",
                    call.present,
                    call.caller,
                    call.world,
                    call.out,
                    call.clipped,
                    call.clamp
                );
            }
            write_dumps(present, w, h, &pixels);
        }
        Err(e) => log!("UI dump failed: {e}"),
    }
}

fn write_dumps(present: u64, w: u32, h: u32, pixels: &[u8]) {
    let Some(session) = monaka_producer::Session::get() else { return };
    let at = |x: u32, y: u32| &pixels[((y * w + x) * 4) as usize..][..4];
    // Premultiplied colour over mid grey, and the coverage.
    let colour = bmp::rgb(w, h, |x, y| {
        let p = at(x, y);
        let a = p[3] as u32;
        std::array::from_fn(|c| (p[c] as u32 + 128 * (255 - a) / 255).min(255) as u8)
    });
    let alpha = bmp::rgb(w, h, |x, y| [at(x, y)[3]; 3]);
    for (name, bytes) in [(format!("ui-{present}.bmp"), colour), (format!("ui-{present}-alpha.bmp"), alpha)] {
        if let Err(e) = std::fs::write(session.path(&name), bytes) {
            log!("UI dump {name}: {e}");
        }
    }
}
