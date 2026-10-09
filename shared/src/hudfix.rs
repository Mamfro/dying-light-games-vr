//! The dynamic HUD's pieces taken out of an eye frame that has the HUD drawn in (alternate-eye
//! stereo, where no warp lays the HUD over the eyes): within each piece's rectangle the world is
//! recovered from the UI layer, and solid HUD pixels are filled from the row's nearest pixel that
//! is not solid (`hudfix12.hlsl`). The frame is a swapchain back buffer (no unordered access), so
//! the rectangles are copied out, fixed into a scratch texture and copied back, on the game queue
//! before the eye is taken.

use monaka_channel::d3d12::compute::{self, DescriptorHeap, Table, srv_desc, uav_desc};
use monaka_channel::d3d12::{self, Recorder, barrier, subresource};
use monaka_producer::log;
use std::ffi::c_void;
use std::sync::Mutex;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
use windows::core::s;

const SOURCE: &str = include_str!("hudfix12.hlsl");

#[repr(C)]
#[derive(Clone, Copy)]
struct Constants {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    rect_width: u32,
    rect_height: u32,
    pad: [u32; 2],
}

struct Fix {
    device: ID3D12Device,
    root: ID3D12RootSignature,
    pipeline: ID3D12PipelineState,
    heap: DescriptorHeap,
    recorder: Recorder,
    /// A copy of the frame's rectangles (read) and the fixed rectangles (written), the frame's size and format.
    source: Option<ID3D12Resource>,
    fixed: Option<ID3D12Resource>,
    size: (u32, u32),
    format: DXGI_FORMAT,
    viewed: Option<(ID3D12Resource, ID3D12Resource, ID3D12Resource)>,
}

static FIX: Mutex<Option<Fix>> = Mutex::new(None);
static FAILED: Mutex<bool> = Mutex::new(false);

impl Fix {
    fn new(device: &ID3D12Device) -> Result<Self, String> {
        let root = compute::root_signature(device, (size_of::<Constants>() / 4) as u32, &[Table { srvs: 2, uavs: 0 }, Table { srvs: 0, uavs: 1 }])?;
        let pipeline = compute::shader_pipeline(device, &root, &monaka_core::hud::with_hlsl(SOURCE), "hudfix12.hlsl", s!("Fix"))?;
        let heap = DescriptorHeap::new(device, 3)?;
        let recorder = Recorder::new(device, 4).map_err(|e| format!("command lists: {e}"))?;
        Ok(Self { device: device.clone(), root, pipeline, heap, recorder, source: None, fixed: None, size: (0, 0), format: DXGI_FORMAT(0), viewed: None })
    }

    fn ensure(&mut self, desc: &D3D12_RESOURCE_DESC) -> Result<(), String> {
        let size = (desc.Width as u32, desc.Height);
        if self.source.is_some() && self.size == size && self.format == desc.Format {
            return Ok(());
        }
        let source = d3d12::texture(&self.device, size.0, size.1, desc.Format, D3D12_RESOURCE_FLAG_NONE).map_err(|e| format!("frame copy: {e}"))?;
        let fixed = d3d12::texture(&self.device, size.0, size.1, desc.Format, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS).map_err(|e| format!("fixed copy: {e}"))?;
        (self.source, self.fixed, self.size, self.format, self.viewed) = (Some(source), Some(fixed), size, desc.Format, None);
        Ok(())
    }

    fn view(&mut self, ui: &ID3D12Resource) {
        let (Some(source), Some(fixed)) = (self.source.clone(), self.fixed.clone()) else { return };
        if self.viewed.as_ref().is_some_and(|(u, s, f)| u == ui && *s == source && *f == fixed) {
            return;
        }
        // SAFETY: views of live textures into our own heap.
        unsafe {
            self.device.CreateShaderResourceView(ui, Some(&srv_desc(windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM)), self.heap.cpu(0));
            self.device.CreateShaderResourceView(&source, Some(&srv_desc(self.format)), self.heap.cpu(1));
            self.device.CreateUnorderedAccessView(&fixed, None, Some(&uav_desc(self.format)), self.heap.cpu(2));
        }
        self.viewed = Some((ui.clone(), source, fixed));
    }
}

/// A rectangle (x, y, width, height) clamped to the frame and whole pixels.
fn clamp(rect: [f32; 4], size: (u32, u32)) -> Option<D3D12_BOX> {
    let x0 = rect[0].max(0.0).floor() as u32;
    let y0 = rect[1].max(0.0).floor() as u32;
    let x1 = ((rect[0] + rect[2]).ceil() as u32).min(size.0);
    let y1 = ((rect[1] + rect[3]).ceil() as u32).min(size.1);
    (x1 > x0 && y1 > y0).then_some(D3D12_BOX { left: x0, top: y0, front: 0, right: x1, bottom: y1, back: 1 })
}

/// Takes the pieces `rects` (x, y, width, height in frame pixels) out of `frame` (in `state`,
/// a back buffer) using the UI layer `ui` (in COMMON), on `queue`. Returns whether it was recorded.
pub fn take_out(queue: &ID3D12CommandQueue, frame: &ID3D12Resource, state: D3D12_RESOURCE_STATES, ui: &ID3D12Resource, rects: &[[f32; 4]]) -> bool {
    if rects.is_empty() || *FAILED.lock().unwrap_or_else(|e| e.into_inner()) {
        return false;
    }
    let mut slot = FIX.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        match d3d12::device_of(queue).map_err(|e| e.to_string()).and_then(|device| Fix::new(&device)) {
            Ok(fix) => *slot = Some(fix),
            Err(why) => {
                log!("dynamic HUD: pieces stay in the eyes: {why}");
                *FAILED.lock().unwrap_or_else(|e| e.into_inner()) = true;
                return false;
            }
        }
    }
    let fix = slot.as_mut().expect("made above");
    // SAFETY: reads a resource descriptor.
    let desc = unsafe { frame.GetDesc() };
    if let Err(why) = fix.ensure(&desc) {
        monaka_producer::log_first!(3, "dynamic HUD: pieces stay in the eyes: {why}");
        return false;
    }
    fix.view(ui);
    let (Some(source), Some(fixed)) = (fix.source.clone(), fix.fixed.clone()) else { return false };
    let Ok(Some(list)) = fix.recorder.begin() else { return false };
    let boxes: Vec<D3D12_BOX> = rects.iter().filter_map(|r| clamp(*r, fix.size)).collect();
    if boxes.is_empty() {
        let _ = fix.recorder.submit(queue);
        return false;
    }
    const SRV: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
    // SAFETY: records copies and compute work on our own list with our own heap, root signature
    // and pipeline; every resource is live until the list has executed (the recorder's fence).
    unsafe {
        barrier(&list, frame, state, D3D12_RESOURCE_STATE_COPY_SOURCE);
        barrier(&list, &source, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST);
        for b in &boxes {
            list.CopyTextureRegion(&subresource(&source, 0), b.left, b.top, 0, &subresource(frame, 0), Some(b));
        }
        barrier(&list, &source, D3D12_RESOURCE_STATE_COPY_DEST, SRV);
        barrier(&list, ui, D3D12_RESOURCE_STATE_COMMON, SRV);
        barrier(&list, &fixed, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        list.SetDescriptorHeaps(&[Some(fix.heap.heap().clone())]);
        list.SetComputeRootSignature(&fix.root);
        list.SetPipelineState(&fix.pipeline);
        list.SetComputeRootDescriptorTable(1, fix.heap.gpu(0));
        list.SetComputeRootDescriptorTable(2, fix.heap.gpu(2));
        for b in &boxes {
            let c = Constants { width: fix.size.0, height: fix.size.1, x: b.left, y: b.top, rect_width: b.right - b.left, rect_height: b.bottom - b.top, pad: [0; 2] };
            list.SetComputeRoot32BitConstants(0, 8, &c as *const Constants as *const c_void, 0);
            list.Dispatch((b.right - b.left).div_ceil(8), (b.bottom - b.top).div_ceil(8), 1);
        }
        barrier(&list, &fixed, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE);
        barrier(&list, frame, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COPY_DEST);
        for b in &boxes {
            list.CopyTextureRegion(&subresource(frame, 0), b.left, b.top, 0, &subresource(&fixed, 0), Some(b));
        }
        barrier(&list, frame, D3D12_RESOURCE_STATE_COPY_DEST, state);
        barrier(&list, &fixed, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON);
        barrier(&list, ui, SRV, D3D12_RESOURCE_STATE_COMMON);
        barrier(&list, &source, SRV, D3D12_RESOURCE_STATE_COMMON);
    }
    match fix.recorder.submit(queue) {
        Ok(_) => true,
        Err(e) => {
            monaka_producer::log_first!(3, "dynamic HUD: pieces stay in the eyes: {e}");
            false
        }
    }
}

/// The end of a run: waits for the GPU and lets the textures go.
pub fn close() {
    if let Some(fix) = FIX.lock().unwrap_or_else(|e| e.into_inner()).take() {
        fix.recorder.idle(2000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;

    #[test]
    fn the_shader_compiles_and_the_fix_initialises() {
        compute::compile(&monaka_core::hud::with_hlsl(SOURCE), "hudfix12.hlsl", s!("Fix")).unwrap();
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: creates a device on the default adapter.
        unsafe { D3D12CreateDevice(None::<&windows::core::IUnknown>, D3D_FEATURE_LEVEL_11_0, &mut device) }.unwrap();
        Fix::new(&device.unwrap()).unwrap();
        assert_eq!(clamp([10.4, 5.6, 20.0, 10.0], (100, 100)).map(|b| (b.left, b.top, b.right, b.bottom)), Some((10, 5, 31, 16)));
        assert!(clamp([-50.0, 0.0, 10.0, 10.0], (100, 100)).is_none());
    }
}
