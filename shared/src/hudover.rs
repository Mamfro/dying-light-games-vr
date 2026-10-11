//! The HUD layer laid back over an eye frame (alternate-eye stereo with the HUD kept out of the
//! frame by [`crate::hudlayer`]): the game's HUD, menus and wheels, drawn for the whole screen,
//! come back in a rectangle of the eye at the HUD's distance and size ([`monaka_core::hud`]'s
//! placement), so they float in front of the player instead of covering the eye. The crosshair is
//! drawn at the game's aim point, the dynamic HUD's pieces (on the hands) are left out, and a dim
//! the game lays over the whole screen (behind a wheel) stays over the whole eye instead of
//! shrinking into a box (`hudover12.hlsl`). The frame is a swapchain back buffer (no unordered
//! access), so it is copied out, laid over into a scratch texture and copied back, on the game
//! queue before the eye is taken: the eye and the monitor both show it.

use monaka_channel::d3d12::compute::{self, DescriptorHeap, Table, srv_desc, uav_desc};
use monaka_channel::d3d12::{self, Recorder, barrier};
use monaka_producer::log;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_R8G8B8A8_UNORM};
use windows::core::s;

const SOURCE: &str = include_str!("hudover12.hlsl");
/// Most pieces left out.
const PIECES: usize = 5;

/// How the layer goes over one frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Params {
    /// Where the layer goes (x, y, width, height, the frame's pixels).
    pub rect: [f32; 4],
    /// The game's aim point from the frame's centre, pixels (x right, y down).
    pub aim: [f32; 2],
    /// Radius around the layer's centre drawn at the aim point instead (crosshair, prompts), pixels.
    pub cross_radius: f32,
    /// Rectangles of the layer (x, y, width, height; frame pixels) left out: the pieces on the hands.
    pub pieces: [[f32; 4]; PIECES],
    pub piece_count: usize,
}

impl Params {
    /// The pieces `rects` (at most five) left out.
    pub fn leaving_out(mut self, rects: &[[f32; 4]]) -> Self {
        self.piece_count = rects.len().min(PIECES);
        self.pieces[..self.piece_count].copy_from_slice(&rects[..self.piece_count]);
        self
    }
}

/// The shader's constants (root constants; `Piece` starts on a 16-byte boundary).
#[repr(C)]
#[derive(Clone, Copy)]
struct Constants {
    width: u32,
    height: u32,
    cross_radius: f32,
    pieces: f32,
    rect: [f32; 4],
    aim: [f32; 2],
    pad: [f32; 2],
    piece_rects: [[f32; 4]; PIECES],
}

struct Over {
    device: ID3D12Device,
    root: ID3D12RootSignature,
    pipeline: ID3D12PipelineState,
    heap: DescriptorHeap,
    recorder: Recorder,
    /// A copy of the frame (read) and the frame with the HUD laid over (written), the frame's size and format.
    source: Option<ID3D12Resource>,
    out: Option<ID3D12Resource>,
    size: (u32, u32),
    format: DXGI_FORMAT,
    viewed: Option<(ID3D12Resource, ID3D12Resource, ID3D12Resource)>,
}

static OVER: Mutex<Option<Over>> = Mutex::new(None);
static FAILED: Mutex<bool> = Mutex::new(false);
static LAID: AtomicU64 = AtomicU64::new(0);

impl Over {
    fn new(device: &ID3D12Device) -> Result<Self, String> {
        let root = compute::root_signature(device, (size_of::<Constants>() / 4) as u32, &[Table { srvs: 2, uavs: 0 }, Table { srvs: 0, uavs: 1 }])?;
        let pipeline = compute::shader_pipeline(device, &root, &monaka_core::hud::with_hlsl(SOURCE), "hudover12.hlsl", s!("Over"))?;
        let heap = DescriptorHeap::new(device, 3)?;
        let recorder = Recorder::new(device, 4).map_err(|e| format!("command lists: {e}"))?;
        Ok(Self { device: device.clone(), root, pipeline, heap, recorder, source: None, out: None, size: (0, 0), format: DXGI_FORMAT(0), viewed: None })
    }

    fn ensure(&mut self, desc: &D3D12_RESOURCE_DESC) -> Result<(), String> {
        let size = (desc.Width as u32, desc.Height);
        if self.source.is_some() && self.size == size && self.format == desc.Format {
            return Ok(());
        }
        let source = d3d12::texture(&self.device, size.0, size.1, desc.Format, D3D12_RESOURCE_FLAG_NONE).map_err(|e| format!("frame copy: {e}"))?;
        let out = d3d12::texture(&self.device, size.0, size.1, desc.Format, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS).map_err(|e| format!("laid-over copy: {e}"))?;
        (self.source, self.out, self.size, self.format, self.viewed) = (Some(source), Some(out), size, desc.Format, None);
        Ok(())
    }

    fn view(&mut self, ui: &ID3D12Resource) {
        let (Some(source), Some(out)) = (self.source.clone(), self.out.clone()) else { return };
        if self.viewed.as_ref().is_some_and(|(u, s, o)| u == ui && *s == source && *o == out) {
            return;
        }
        // SAFETY: views of live textures into our own heap.
        unsafe {
            self.device.CreateShaderResourceView(&source, Some(&srv_desc(self.format)), self.heap.cpu(0));
            self.device.CreateShaderResourceView(ui, Some(&srv_desc(DXGI_FORMAT_R8G8B8A8_UNORM)), self.heap.cpu(1));
            self.device.CreateUnorderedAccessView(&out, None, Some(&uav_desc(self.format)), self.heap.cpu(2));
        }
        self.viewed = Some((ui.clone(), source, out));
    }
}

/// Lays the UI layer `ui` (in COMMON, the frame's size) over `frame` (in `state`, a back buffer)
/// as `params` say, on `queue`. Returns whether it was recorded (otherwise the frame has no HUD).
pub fn lay(queue: &ID3D12CommandQueue, frame: &ID3D12Resource, state: D3D12_RESOURCE_STATES, ui: &ID3D12Resource, params: &Params) -> bool {
    if *FAILED.lock().unwrap_or_else(|e| e.into_inner()) {
        return false;
    }
    let mut slot = OVER.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        match d3d12::device_of(queue).map_err(|e| e.to_string()).and_then(|device| Over::new(&device)) {
            Ok(over) => *slot = Some(over),
            Err(why) => {
                log!("HUD lay-over unavailable: {why}");
                *FAILED.lock().unwrap_or_else(|e| e.into_inner()) = true;
                return false;
            }
        }
    }
    let over = slot.as_mut().expect("made above");
    // SAFETY: reads resource descriptors.
    let (desc, ui_desc) = unsafe { (frame.GetDesc(), ui.GetDesc()) };
    if ui_desc.Width != desc.Width || ui_desc.Height != desc.Height {
        return false;
    }
    if let Err(why) = over.ensure(&desc) {
        monaka_producer::log_first!(3, "HUD lay-over not run: {why}");
        return false;
    }
    over.view(ui);
    let (Some(source), Some(out)) = (over.source.clone(), over.out.clone()) else { return false };
    let Ok(Some(list)) = over.recorder.begin() else { return false };
    let mut piece_rects = [[0.0; 4]; PIECES];
    piece_rects[..params.piece_count].copy_from_slice(&params.pieces[..params.piece_count]);
    let constants = Constants {
        width: over.size.0,
        height: over.size.1,
        cross_radius: params.cross_radius,
        pieces: params.piece_count as f32,
        rect: params.rect,
        aim: params.aim,
        pad: [0.0; 2],
        piece_rects,
    };
    const SRV: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
    d3d12::copy_whole(&list, &source, frame, state);
    // SAFETY: records copies and compute work on our own list with our own heap, root signature
    // and pipeline; every resource is live until the list has executed (the recorder's fence).
    unsafe {
        barrier(&list, &source, D3D12_RESOURCE_STATE_COMMON, SRV);
        barrier(&list, ui, D3D12_RESOURCE_STATE_COMMON, SRV);
        barrier(&list, &out, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        list.SetDescriptorHeaps(&[Some(over.heap.heap().clone())]);
        list.SetComputeRootSignature(&over.root);
        list.SetPipelineState(&over.pipeline);
        list.SetComputeRootDescriptorTable(1, over.heap.gpu(0));
        list.SetComputeRootDescriptorTable(2, over.heap.gpu(2));
        list.SetComputeRoot32BitConstants(0, (size_of::<Constants>() / 4) as u32, &constants as *const Constants as *const c_void, 0);
        list.Dispatch(over.size.0.div_ceil(8), over.size.1.div_ceil(8), 1);
        barrier(&list, &out, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE);
        barrier(&list, frame, state, D3D12_RESOURCE_STATE_COPY_DEST);
        list.CopyResource(frame, &out);
        barrier(&list, frame, D3D12_RESOURCE_STATE_COPY_DEST, state);
        barrier(&list, &out, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON);
        barrier(&list, ui, SRV, D3D12_RESOURCE_STATE_COMMON);
        barrier(&list, &source, SRV, D3D12_RESOURCE_STATE_COMMON);
    }
    match over.recorder.submit(queue) {
        Ok(_) => {
            let n = LAID.fetch_add(1, Relaxed);
            if n == 0 || n.is_multiple_of(600) {
                log!("HUD lay-over {n}: the layer in {:.0?} of {}x{}, {} pieces left out", params.rect, over.size.0, over.size.1, params.piece_count);
            }
            true
        }
        Err(e) => {
            monaka_producer::log_first!(3, "HUD lay-over not run: {e}");
            false
        }
    }
}

/// The end of a run: waits for the GPU and lets the textures go.
pub fn close() {
    if let Some(over) = OVER.lock().unwrap_or_else(|e| e.into_inner()).take() {
        over.recorder.idle(2000);
    }
    let laid = LAID.load(Relaxed);
    if laid > 0 {
        log!("HUD lay-over: {laid} frames");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;

    #[test]
    fn the_shader_compiles_and_the_lay_over_initialises() {
        compute::compile(&monaka_core::hud::with_hlsl(SOURCE), "hudover12.hlsl", s!("Over")).unwrap();
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: creates a device on the default adapter.
        unsafe { D3D12CreateDevice(None::<&windows::core::IUnknown>, D3D_FEATURE_LEVEL_11_0, &mut device) }.unwrap();
        Over::new(&device.unwrap()).unwrap();
        assert_eq!(size_of::<Constants>(), 128, "the shader's constant buffer");
        let params = Params::default().leaving_out(&[[1.0; 4]; 7]);
        assert_eq!(params.piece_count, 5, "at most five pieces");
    }
}
