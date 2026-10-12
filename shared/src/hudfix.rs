//! The dynamic HUD's pieces taken out of an eye frame that has the HUD drawn in (alternate-eye
//! stereo, where no warp lays the HUD over the eyes): within each piece's rectangle the world is
//! recovered from the UI layer, and solid HUD pixels are filled from the row's nearest pixel that
//! is not solid (`hudfix12.hlsl`). The frame is a swapchain back buffer (no unordered access), so
//! the rectangles are copied out, fixed into a scratch texture and copied back, on the game queue
//! before the eye is taken.

use crate::framepass::{FramePass, Lazy};
use monaka_channel::d3d12::{barrier, subresource};
use std::ffi::c_void;
use windows::Win32::Graphics::Direct3D12::*;
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

static FIX: Lazy<FramePass> = Lazy::new();

fn make(device: &ID3D12Device) -> Result<FramePass, String> {
    FramePass::new(device, (size_of::<Constants>() / 4) as u32, SOURCE, "hudfix12.hlsl", s!("Fix"), false)
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
    if rects.is_empty() {
        return false;
    }
    FIX.with(queue, make, "dynamic HUD: pieces stay in the eyes", |fix| take_out_with(fix, queue, frame, state, ui, rects)).unwrap_or(false)
}

fn take_out_with(fix: &mut FramePass, queue: &ID3D12CommandQueue, frame: &ID3D12Resource, state: D3D12_RESOURCE_STATES, ui: &ID3D12Resource, rects: &[[f32; 4]]) -> bool {
    // SAFETY: reads a resource descriptor.
    let desc = unsafe { frame.GetDesc() };
    if let Err(why) = fix.ensure(&desc) {
        monaka_producer::log_first!(3, "dynamic HUD: pieces stay in the eyes: {why}");
        return false;
    }
    fix.view(ui);
    let (Some(source), Some(fixed)) = (fix.source.clone(), fix.out.clone()) else { return false };
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
        fix.bind(&list);
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
    if let Some(fix) = FIX.take() {
        fix.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;

    #[test]
    fn the_shader_compiles_and_the_fix_initialises() {
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: creates a device on the default adapter.
        unsafe { D3D12CreateDevice(None::<&windows::core::IUnknown>, D3D_FEATURE_LEVEL_11_0, &mut device) }.unwrap();
        make(&device.unwrap()).unwrap();
        assert_eq!(clamp([10.4, 5.6, 20.0, 10.0], (100, 100)).map(|b| (b.left, b.top, b.right, b.bottom)), Some((10, 5, 31, 16)));
        assert!(clamp([-50.0, 0.0, 10.0, 10.0], (100, 100)).is_none());
    }
}
