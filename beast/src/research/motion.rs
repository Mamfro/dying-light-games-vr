//! `probe_motion=1`: the units and direction of the game's motion vectors, measured (read-only). One
//! frame's depth and motion vectors are copied into CPU-readable buffers when the game tags them for
//! DLSS (plain copies on its command list, as DL2's frame generation does), and compared with the
//! motion the camera alone gives each pixel from its depth and that frame's `clipToPrevClip`. With
//! the player still and a fixed head, the camera (the swap to the other eye) is the only motion, so
//! a least-squares fit of the game's vectors against it gives their scale and sign.
//!
//! The copies and the camera come from the Streamline hooks ([`super::streamline`]).

use super::options;
use monaka_channel::d3d12::{self, barrier};
use monaka_core::half;
use monaka_hook::mem;
use monaka_producer::log;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct3D12::*;

/// The frame's camera as Streamline gets it: `clipToPrevClip` (row vectors) and `mvecScale`.
#[derive(Clone, Copy)]
struct Camera {
    clip_to_prev: [f32; 16],
    mvec_scale: [f32; 2],
}

struct Readback {
    buffer: ID3D12Resource,
    footprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
    /// Bytes per texel of the copied plane.
    texel: usize,
    width: u32,
    height: u32,
}

struct Probe {
    /// Tag calls to let pass first (the frame's eye must be settled).
    skip: u32,
    camera: Option<Camera>,
    depth: Option<Readback>,
    motion: Option<Readback>,
    taken: Option<(Instant, Camera)>,
    done: bool,
}

static PROBE: Mutex<Option<Probe>> = Mutex::new(None);

/// The Streamline inputs are hooked: the probe arms.
pub fn arm() {
    if !options().motion {
        return;
    }
    *PROBE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Probe { skip: 120, camera: None, depth: None, motion: None, taken: None, done: false });
    log!("motion vector probe armed");
}

/// This frame's camera constants (`sl::Constants`).
pub fn at_constants(constants: usize) {
    let mut probe = PROBE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = probe.as_mut().filter(|p| p.taken.is_none()) else { return };
    if let (Some(clip_to_prev), Some(mvec_scale)) = (mem::read::<[f32; 16]>(constants + 224), mem::read::<[f32; 2]>(constants + 360)) {
        p.camera = Some(Camera { clip_to_prev, mvec_scale });
    }
}

/// The copyable layout of plane 0 of a tagged texture: depth (R32G8X24, whose plane 0 is the 32-bit
/// depth) or motion vectors (16-bit RGBA), rows aligned to 256 bytes; and its bytes per texel.
fn footprint(desc: &D3D12_RESOURCE_DESC) -> Result<(D3D12_PLACED_SUBRESOURCE_FOOTPRINT, usize), String> {
    use windows::Win32::Graphics::Dxgi::Common::*;
    let (format, bytes) = match desc.Format {
        DXGI_FORMAT_R32G8X24_TYPELESS | DXGI_FORMAT_D32_FLOAT_S8X24_UINT | DXGI_FORMAT_R32_FLOAT_X8X24_TYPELESS => (DXGI_FORMAT_R32_TYPELESS, 4),
        DXGI_FORMAT_R32_TYPELESS | DXGI_FORMAT_D32_FLOAT | DXGI_FORMAT_R32_FLOAT => (DXGI_FORMAT_R32_TYPELESS, 4),
        DXGI_FORMAT_R16G16B16A16_TYPELESS | DXGI_FORMAT_R16G16B16A16_FLOAT => (DXGI_FORMAT_R16G16B16A16_TYPELESS, 8),
        other => return Err(format!("format {} not handled", other.0)),
    };
    let row_pitch = (desc.Width as u32 * bytes).next_multiple_of(D3D12_TEXTURE_DATA_PITCH_ALIGNMENT);
    let footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
        Offset: 0,
        Footprint: D3D12_SUBRESOURCE_FOOTPRINT { Format: format, Width: desc.Width as u32, Height: desc.Height, Depth: 1, RowPitch: row_pitch },
    };
    Ok((footprint, bytes as usize))
}

fn readback(device: &ID3D12Device, resource: &ID3D12Resource) -> Result<Readback, String> {
    // SAFETY: a plain descriptor read.
    let desc = unsafe { resource.GetDesc() };
    let (footprint, texel) = footprint(&desc)?;
    let total = footprint.Footprint.RowPitch as u64 * desc.Height as u64;
    let buffer = d3d12::readback_buffer(device, total).map_err(|e| format!("{e} ({}x{} format {}, {total} bytes)", desc.Width, desc.Height, desc.Format.0))?;
    Ok(Readback { buffer, footprint, texel, width: desc.Width as u32, height: desc.Height })
}

/// The game tags `resource` (in `state`) as buffer `kind` on `list`: depth (0) and motion vectors
/// (1) of one frame are copied once.
pub fn at_tag(list: &ID3D12GraphicsCommandList, kind: u32, resource: &ID3D12Resource, state: D3D12_RESOURCE_STATES) {
    if kind > 1 {
        return;
    }
    let mut probe = PROBE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = probe.as_mut().filter(|p| p.taken.is_none()) else { return };
    if kind == 0 && p.skip > 0 {
        p.skip -= 1;
        return;
    }
    if p.skip > 0 || (kind == 1 && p.depth.is_none()) {
        return;
    }
    let made = d3d12::device_of(resource).map_err(|e| e.to_string()).and_then(|device| readback(&device, resource));
    let target = match made {
        Ok(target) => target,
        Err(e) => {
            log!("motion vector probe: no readback buffer ({e})");
            p.taken = Some((Instant::now(), p.camera.unwrap_or(Camera { clip_to_prev: [0.0; 16], mvec_scale: [0.0; 2] })));
            p.done = true;
            return;
        }
    };
    let destination = d3d12::placed(&target.buffer, target.footprint);
    barrier(list, resource, state, D3D12_RESOURCE_STATE_COPY_SOURCE);
    // SAFETY: records a copy of the game's tagged texture (plane 0) into our buffer on the game's
    // list, between transitions that leave the texture in its tagged state.
    unsafe { list.CopyTextureRegion(&destination, 0, 0, 0, &d3d12::subresource(resource, 0), None) };
    barrier(list, resource, D3D12_RESOURCE_STATE_COPY_SOURCE, state);
    if kind == 0 {
        p.depth = Some(target);
    } else {
        p.motion = Some(target);
        p.taken = p.camera.map(|camera| (Instant::now(), camera));
        log!("motion vector probe: depth and motion vectors of one frame copied");
    }
}

/// A second after the copies (reporter thread): the fit.
pub fn report() {
    let mut probe = PROBE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = probe.as_mut().filter(|p| !p.done) else { return };
    let (Some((taken, camera)), Some(depth), Some(motion)) = (p.taken, &p.depth, &p.motion) else { return };
    if taken.elapsed() < Duration::from_secs(1) {
        return;
    }
    p.done = true;
    // The copies finished a second ago.
    let map = |r: &Readback| d3d12::read_rows(&r.buffer, &r.footprint, r.height, r.width as usize * r.texel).ok();
    let (Some(depth_bytes), Some(motion_bytes)) = (map(depth), map(motion)) else {
        log!("motion vector probe: readback buffers could not be mapped");
        return;
    };
    let (width, height) = (motion.width, motion.height);
    let depth_pitch = depth.width as usize * depth.texel;
    let motion_pitch = motion.width as usize * motion.texel;
    let c = camera.clip_to_prev;
    let (mut game_dot_camera, mut camera_sq, mut game_sq, mut samples) = (0f64, 0f64, 0f64, 0u32);
    let (mut game_sum, mut camera_sum) = ([0f64; 2], [0f64; 2]);
    for y in (4..height).step_by(16) {
        for x in (4..width).step_by(16) {
            let d_at = y as usize * depth_pitch + x as usize * 4;
            let d = f32::from_le_bytes(depth_bytes[d_at..d_at + 4].try_into().expect("4 bytes"));
            let m_at = y as usize * motion_pitch + x as usize * 8;
            let mv = [half::to_f32(u16::from_le_bytes([motion_bytes[m_at], motion_bytes[m_at + 1]])), half::to_f32(u16::from_le_bytes([motion_bytes[m_at + 2], motion_bytes[m_at + 3]]))];
            if !d.is_finite() || !mv.iter().all(|v| v.is_finite()) {
                continue;
            }
            let ndc = [(x as f32 + 0.5) / width as f32 * 2.0 - 1.0, 1.0 - (y as f32 + 0.5) / height as f32 * 2.0];
            let clip = [ndc[0], ndc[1], d, 1.0];
            let prev: [f32; 4] = std::array::from_fn(|j| (0..4).map(|i| clip[i] * c[i * 4 + j]).sum());
            if prev[3].abs() < 1e-6 {
                continue;
            }
            // The camera's motion in UV (y down), from now to the previous frame.
            let camera_uv = [(prev[0] / prev[3] - ndc[0]) * 0.5, -(prev[1] / prev[3] - ndc[1]) * 0.5];
            let game_uv = [mv[0] * camera.mvec_scale[0], mv[1] * camera.mvec_scale[1]];
            for i in 0..2 {
                game_dot_camera += (game_uv[i] * camera_uv[i]) as f64;
                camera_sq += (camera_uv[i] * camera_uv[i]) as f64;
                game_sq += (game_uv[i] * game_uv[i]) as f64;
                game_sum[i] += game_uv[i] as f64;
                camera_sum[i] += camera_uv[i] as f64;
            }
            samples += 1;
        }
    }
    if samples == 0 || camera_sq == 0.0 {
        log!("motion vector probe: no usable samples");
        return;
    }
    let k = game_dot_camera / camera_sq;
    let residual = (game_sq - 2.0 * k * game_dot_camera + k * k * camera_sq).max(0.0);
    let n = samples as f64;
    log!(
        "motion vector probe ({width}x{height}, mvecScale {:?}, {samples} samples): game = {k:.3} x camera (RMS of the game's {:.5}, of what is left {:.5} in UV); mean game ({:.5}, {:.5}) camera ({:.5}, {:.5})",
        camera.mvec_scale,
        (game_sq / n).sqrt(),
        (residual / n).sqrt(),
        game_sum[0] / n,
        game_sum[1] / n,
        camera_sum[0] / n,
        camera_sum[1] / n,
    );
}
