//! A compute pass over a swapchain frame, which has no unordered access: the frame (or its
//! rectangles) copied out, a shader run from that copy and the UI layer into a scratch texture,
//! the result copied back, on the game's queue before the eye is taken. The HUD's passes over the
//! eyes ([`crate::hudover`], [`crate::hudfix`]) are this with their own shader and constants.

use monaka_channel::d3d12::compute::{self, DescriptorHeap, Table, srv_desc, uav_desc};
use monaka_channel::d3d12::{self, Recorder};
use monaka_core::sync::Locked;
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::*};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_R8G8B8A8_UNORM};
use windows::core::PCSTR;

pub struct FramePass {
    device: ID3D12Device,
    root: ID3D12RootSignature,
    pipeline: ID3D12PipelineState,
    heap: DescriptorHeap,
    pub recorder: Recorder,
    /// A copy of the frame (read) and the pass's result (written), the frame's size and format.
    pub source: Option<ID3D12Resource>,
    pub out: Option<ID3D12Resource>,
    pub size: (u32, u32),
    format: DXGI_FORMAT,
    viewed: Option<(ID3D12Resource, ID3D12Resource, ID3D12Resource)>,
    /// The shader reads the frame copy as its first texture and the layer as its second (else the
    /// other way round).
    frame_first: bool,
}

impl FramePass {
    /// The pass of HLSL `source` (`file`'s, entry `entry`) with `constants` 32-bit root constants;
    /// it reads two textures (`frame_first`: the frame copy, then the layer) and writes one.
    pub fn new(device: &ID3D12Device, constants: u32, source: &str, file: &str, entry: PCSTR, frame_first: bool) -> Result<Self, String> {
        let root = compute::root_signature(device, constants, &[Table { srvs: 2, uavs: 0 }, Table { srvs: 0, uavs: 1 }])?;
        let pipeline = compute::shader_pipeline(device, &root, &monaka_core::hud::with_hlsl(source), file, entry)?;
        let heap = DescriptorHeap::new(device, 3)?;
        let recorder = Recorder::new(device, 4).map_err(|e| format!("command lists: {e}"))?;
        Ok(Self { device: device.clone(), root, pipeline, heap, recorder, source: None, out: None, size: (0, 0), format: DXGI_FORMAT(0), viewed: None, frame_first })
    }

    /// The copy and the result for a frame of `desc`, made again when its size or format changes.
    pub fn ensure(&mut self, desc: &D3D12_RESOURCE_DESC) -> Result<(), String> {
        let size = (desc.Width as u32, desc.Height);
        if self.source.is_some() && self.size == size && self.format == desc.Format {
            return Ok(());
        }
        let source = d3d12::texture(&self.device, size.0, size.1, desc.Format, D3D12_RESOURCE_FLAG_NONE).map_err(|e| format!("frame copy: {e}"))?;
        let out = d3d12::texture(&self.device, size.0, size.1, desc.Format, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS).map_err(|e| format!("result: {e}"))?;
        (self.source, self.out, self.size, self.format, self.viewed) = (Some(source), Some(out), size, desc.Format, None);
        Ok(())
    }

    /// The views of the frame copy, the UI layer `ui` and the result (made again when one changes).
    pub fn view(&mut self, ui: &ID3D12Resource) {
        let (Some(source), Some(out)) = (self.source.clone(), self.out.clone()) else { return };
        if self.viewed.as_ref().is_some_and(|(u, s, o)| u == ui && *s == source && *o == out) {
            return;
        }
        let (frame_slot, ui_slot) = if self.frame_first { (0, 1) } else { (1, 0) };
        // SAFETY: views of live textures into our own heap.
        unsafe {
            self.device.CreateShaderResourceView(&source, Some(&srv_desc(self.format)), self.heap.cpu(frame_slot));
            self.device.CreateShaderResourceView(ui, Some(&srv_desc(DXGI_FORMAT_R8G8B8A8_UNORM)), self.heap.cpu(ui_slot));
            self.device.CreateUnorderedAccessView(&out, None, Some(&uav_desc(self.format)), self.heap.cpu(2));
        }
        self.viewed = Some((ui.clone(), source, out));
    }

    /// The pass's heap, root signature, pipeline and tables set on `list`.
    ///
    /// # Safety
    /// `list` is open for recording, on this pass's device.
    pub unsafe fn bind(&self, list: &ID3D12GraphicsCommandList) {
        // SAFETY: the caller's guarantee; our own heap, root signature and pipeline.
        unsafe {
            list.SetDescriptorHeaps(&[Some(self.heap.heap().clone())]);
            list.SetComputeRootSignature(&self.root);
            list.SetPipelineState(&self.pipeline);
            list.SetComputeRootDescriptorTable(1, self.heap.gpu(0));
            list.SetComputeRootDescriptorTable(2, self.heap.gpu(2));
        }
    }

    /// Waits for the GPU and lets the textures go.
    pub fn close(self) {
        self.recorder.idle(2000);
    }
}

/// A pass made on first use from the game queue's device, and not tried again once that failed.
pub struct Lazy<T> {
    slot: Mutex<Option<T>>,
    failed: AtomicBool,
}

impl<T> Lazy<T> {
    pub const fn new() -> Self {
        Self { slot: Mutex::new(None), failed: AtomicBool::new(false) }
    }

    /// `work` on the pass, made by `make` from `queue`'s device if it is not yet (a failure logged
    /// once after `unavailable`); none when it could not be made.
    pub fn with<R>(&self, queue: &ID3D12CommandQueue, make: impl FnOnce(&ID3D12Device) -> Result<T, String>, unavailable: &str, work: impl FnOnce(&mut T) -> R) -> Option<R> {
        if self.failed.load(Acquire) {
            return None;
        }
        let mut slot = self.slot.locked();
        if slot.is_none() {
            match d3d12::device_of(queue).map_err(|e| e.to_string()).and_then(|device| make(&device)) {
                Ok(made) => *slot = Some(made),
                Err(why) => {
                    log!("{unavailable}: {why}");
                    self.failed.store(true, Release);
                    return None;
                }
            }
        }
        slot.as_mut().map(work)
    }

    /// The pass, if it was made (the end of a run).
    pub fn take(&self) -> Option<T> {
        self.slot.locked().take()
    }
}

impl<T> Default for Lazy<T> {
    fn default() -> Self {
        Self::new()
    }
}
