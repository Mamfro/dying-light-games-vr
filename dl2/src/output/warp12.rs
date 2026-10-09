//! Two eye images from one rendered image and its depth (depth stereo), on the game's D3D12 queue.
//!
//! The eyes are parallel cameras half an IPD either side of the rendered one, with its projection,
//! so with reverse-Z infinite depth d = near / distance the shift is linear in d:
//! pixels = d * ipd/2 * P00 * W / (2 * near). With the game's UI layer (colour and alpha, tagged
//! for DLSS frame generation) the HUD is taken out of the world before the warp (translucent HUD
//! un-blended, solid HUD refilled from the surrounding world) and laid back over each eye unwarped,
//! offset by a small fixed disparity so it floats at a comfortable distance. The game spreads its
//! HUD to the screen edges, so the layout is compacted per axis: elements keep `hud_scale` (size),
//! the inner half is scaled about the centre and the outer half moved in so the HUD edge lands at
//! `hud_spread` (both fractions of the half view). The shader is `warp12.hlsl`.

use monaka_channel::d3d12::compute::{self, DescriptorHeap, Table, srv_desc, uav_desc};
use monaka_channel::d3d12::{self, Recorder, barrier, uav_barrier};
use std::ffi::c_void;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_R32_TYPELESS, DXGI_FORMAT_R32_UINT};
use windows::core::s;

const SOURCE: &str = include_str!("warp12.hlsl");

/// The shader with the shared HUD functions before it.
fn source() -> String {
    monaka_core::hud::with_hlsl(SOURCE)
}

/// The shader's constant buffer (root constants; `Piece` must start on a 16-byte boundary).
#[repr(C)]
#[derive(Clone, Copy)]
struct Constants {
    width: u32,
    height: u32,
    depth_width: u32,
    depth_height: u32,
    scale: f32,
    direction: f32,
    max_shift: u32,
    has_ui: u32,
    /// Where the HUD layer goes in this eye (x, y, width, height).
    hud_rect: [f32; 4],
    marker_count: f32,
    aim_x: f32,
    aim_y: f32,
    cross_radius: f32,
    hud_from_frame: f32,
    grid_offset_x: f32,
    grid_scale_x: f32,
    grid_scale_y: f32,
    grid_offset_y: f32,
    /// How many of `pieces` are set.
    pieces: f32,
    /// The first of this frame's markers in the marker buffer.
    marker_base: f32,
    pad: f32,
    /// Rectangles of the UI layer (x, y, width, height) the HUD lay-over leaves out: the dynamic
    /// HUD's pieces, shown on the hands instead.
    piece_rects: [[f32; 4]; 5],
}

/// A HUD world marker: its box in the UI layer (x, y, width, height; the frame's pixels) and its
/// disparity in pixels (half per eye) at its target's distance. Laid over where the game drew it,
/// each eye's copy moved by the disparity, and left out of the flat HUD.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marker {
    pub rect: [f32; 4],
    pub shift: f32,
}

/// The shader's marker (32 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MarkerGpu {
    rect: [f32; 4],
    shift: f32,
    _pad: [f32; 3],
}

/// Markers per frame at most, and frames in flight the ring holds.
const MARKERS_MOST: usize = 32;
const MARKER_RING: usize = 4;

/// An upload buffer of [`MARKER_RING`] frames' markers, written on the CPU before each frame's
/// work is recorded (the GPU reads a frame's region while the next ones are written).
struct MarkerRing {
    buffer: ID3D12Resource,
    mapped: *mut MarkerGpu,
    next: usize,
}

// SAFETY: the buffer is a D3D12 object (free-threaded); the mapping is used under its owner's lock.
unsafe impl Send for MarkerRing {}

impl MarkerRing {
    fn new(device: &ID3D12Device) -> Result<Self, String> {
        let bytes = (MARKERS_MOST * MARKER_RING * size_of::<MarkerGpu>()) as u64;
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: bytes,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_UPLOAD, ..Default::default() };
        let mut buffer: Option<ID3D12Resource> = None;
        // SAFETY: creates and maps our own upload buffer; it stays mapped for its life.
        unsafe {
            device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_GENERIC_READ, None, &mut buffer).map_err(|e| format!("marker buffer: {e}"))?;
            let buffer = buffer.expect("created");
            let mut mapped: *mut c_void = std::ptr::null_mut();
            buffer.Map(0, None, Some(&mut mapped)).map_err(|e| format!("marker buffer map: {e}"))?;
            Ok(Self { buffer, mapped: mapped as *mut MarkerGpu, next: 0 })
        }
    }

    /// A view of the whole ring into `handle`.
    fn view(&self, device: &ID3D12Device, handle: D3D12_CPU_DESCRIPTOR_HANDLE) {
        let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN,
            ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_SRV { FirstElement: 0, NumElements: (MARKERS_MOST * MARKER_RING) as u32, StructureByteStride: size_of::<MarkerGpu>() as u32, Flags: D3D12_BUFFER_SRV_FLAG_NONE },
            },
        };
        // SAFETY: a view of our own buffer into the caller's heap slot.
        unsafe { device.CreateShaderResourceView(&self.buffer, Some(&desc), handle) };
    }

    /// Writes this frame's markers into the next region: its first element and the count.
    fn write(&mut self, markers: &[Marker]) -> (u32, u32) {
        let region = self.next;
        self.next = (self.next + 1) % MARKER_RING;
        let count = markers.len().min(MARKERS_MOST);
        for (i, m) in markers.iter().take(count).enumerate() {
            // SAFETY: inside the mapped buffer (region and index bounded).
            unsafe { self.mapped.add(region * MARKERS_MOST + i).write(MarkerGpu { rect: m.rect, shift: m.shift, _pad: [0.0; 3] }) };
        }
        ((region * MARKERS_MOST) as u32, count as u32)
    }
}

/// One warp: how far pixels move and where the HUD goes.
#[derive(Clone)]
pub struct Params<'a> {
    /// Pixels of shift per unit of reverse-Z depth.
    pub scale: f32,
    /// No pixel moves further than this (the nearest comfortable distance).
    pub max_shift: u32,
    /// Where the HUD layer goes in each eye's image (x, y, width, height, the frame's pixels),
    /// from [`monaka_core::hud::Placement`].
    pub hud: [[f32; 4]; 2],

    /// The game's aim point relative to the centre, in pixels (x right, y down).
    pub aim: [f32; 2],
    /// Radius around the centre drawn at the aim point (crosshair and prompts).
    pub cross_radius: f32,
    /// Keep the frame's own HUD pixels where the UI layer has HUD (a HUD someone else composited).
    pub hud_from_frame: bool,
    /// The UI layer holds HUD draws kept out of the frame (`eng_chr::hudlayer` redirecting): the
    /// frame has no HUD to leave out or un-blend.
    pub ui_out_of_frame: bool,
    /// The HUD's world markers, laid over at their targets' depth and left out of the flat HUD.
    pub markers: Vec<Marker>,
    /// An image (the frame's shape, in COMMON) for pixels the warp leaves empty.
    pub fallback: Option<&'a ID3D12Resource>,
    /// Make the eye in another grid: eye pixel = frame pixel * scale + offset, per axis
    /// (scale x, offset x, scale y, offset y); zero scales keep the frame's grid.
    pub grid: [f32; 4],
    /// Rectangles of the UI layer (x, y, width, height, frame pixels; empty ones unused) left out
    /// of the HUD laid over the eyes: the dynamic HUD's pieces, which go on the hands instead. The
    /// world under them is still recovered from the layer.
    pub pieces: [[f32; 4]; 5],
}

impl Params<'_> {
    pub fn depth_stereo(scale: f32, max_shift: u32, hud: [[f32; 4]; 2], aim: [f32; 2], cross_radius: f32) -> Self {
        Params { scale, max_shift, hud, aim, cross_radius, hud_from_frame: false, ui_out_of_frame: false, markers: Vec::new(), fallback: None, grid: [0.0; 4], pieces: [[0.0; 4]; 5] }
    }
}

pub struct DepthWarp {
    device: ID3D12Device,
    root: ID3D12RootSignature,
    clear: ID3D12PipelineState,
    splat: ID3D12PipelineState,
    resolve: ID3D12PipelineState,
    overlay: ID3D12PipelineState,
    heap: DescriptorHeap,
    color: Option<ID3D12Resource>,
    depth: Option<ID3D12Resource>,
    keys: [Option<ID3D12Resource>; 2],
    eyes: [Option<ID3D12Resource>; 2],
    ui: Option<ID3D12Resource>,
    fallback: Option<ID3D12Resource>,
    width: u32,
    height: u32,
    depth_width: u32,
    depth_height: u32,
    format: DXGI_FORMAT,
    depth_ready: bool,
    recorder: Recorder,
    markers: MarkerRing,
}

/// Descriptor slots: the colour, depth, UI layer, fallback and markers, then each eye's keys and image.
const SRVS: u32 = 5;

impl DepthWarp {
    pub fn new(device: &ID3D12Device) -> Result<Self, String> {
        // Constants, then the inputs (colour, depth, UI layer, fallback) and one eye's keys and image.
        let root = compute::root_signature(device, (size_of::<Constants>() / 4) as u32, &[Table { srvs: SRVS, uavs: 0 }, Table { srvs: 0, uavs: 2 }])?;
        let pipeline = |entry| compute::shader_pipeline(device, &root, &source(), "warp12.hlsl", entry);
        let (clear, splat, resolve, overlay) = (pipeline(s!("Clear"))?, pipeline(s!("Splat"))?, pipeline(s!("Resolve"))?, pipeline(s!("Overlay"))?);
        let heap = DescriptorHeap::new(device, SRVS + 4)?;
        let recorder = Recorder::new(device, 4).map_err(|e| format!("command lists: {e}"))?;
        let markers = MarkerRing::new(device)?;
        let warp = Self {
            device: device.clone(),
            root,
            clear,
            splat,
            resolve,
            overlay,
            heap,
            color: None,
            depth: None,
            keys: [None, None],
            eyes: [None, None],
            ui: None,
            fallback: None,
            width: 0,
            height: 0,
            depth_width: 0,
            depth_height: 0,
            format: DXGI_FORMAT(0),
            depth_ready: false,
            recorder,
            markers,
        };
        // No UI layer until one is tagged, nor a fallback image until one is given.
        // SAFETY: null views into our own heap.
        unsafe {
            device.CreateShaderResourceView(None, Some(&srv_desc(DXGI_FORMAT_R8G8B8A8_UNORM)), warp.heap.cpu(2));
            device.CreateShaderResourceView(None, Some(&srv_desc(DXGI_FORMAT_R8G8B8A8_UNORM)), warp.heap.cpu(3));
        }
        warp.markers.view(device, warp.heap.cpu(4));
        Ok(warp)
    }

    pub fn eye(&self, eye: usize) -> Option<&ID3D12Resource> {
        self.eyes[eye].as_ref()
    }

    pub fn ui(&self) -> Option<&ID3D12Resource> {
        self.ui.as_ref()
    }

    /// Recorded into the game's own command list when it tags its depth for DLSS, so the copy runs
    /// after the depth is complete; the depth plane goes to a private R32 texture.
    pub fn copy_depth(&mut self, list: &ID3D12GraphicsCommandList, source: &ID3D12Resource, state: D3D12_RESOURCE_STATES) -> windows::core::Result<()> {
        // SAFETY: reads a resource descriptor.
        let desc = unsafe { source.GetDesc() };
        if self.depth.is_none() || desc.Width as u32 != self.depth_width || desc.Height != self.depth_height {
            self.depth = None;
            self.depth_ready = false;
            let depth = d3d12::texture(&self.device, desc.Width as u32, desc.Height, DXGI_FORMAT_R32_TYPELESS, D3D12_RESOURCE_FLAG_NONE)?;
            (self.depth_width, self.depth_height) = (desc.Width as u32, desc.Height);
            // SAFETY: a view of our own texture into our own heap.
            unsafe { self.device.CreateShaderResourceView(&depth, Some(&srv_desc(DXGI_FORMAT_R32_FLOAT)), self.heap.cpu(1)) };
            self.depth = Some(depth);
        }
        d3d12::copy_plane0(list, self.depth.as_ref().expect("created above"), source, state);
        self.depth_ready = true;
        Ok(())
    }

    /// A UI layer made elsewhere (`eng_chr::hudlayer`, in COMMON at the present) used as the
    /// frame's, until the game tags one of its own.
    pub fn use_layer(&mut self, layer: &ID3D12Resource) {
        if self.ui.as_ref() == Some(layer) {
            return;
        }
        // SAFETY: a view of a live texture into our own heap.
        unsafe { self.device.CreateShaderResourceView(layer, Some(&srv_desc(DXGI_FORMAT_R8G8B8A8_UNORM)), self.heap.cpu(2)) };
        self.ui = Some(layer.clone());
    }

    /// The UI layer (tagged for DLSS frame generation), copied into a private texture on the
    /// game's list; the copy ends in COMMON.
    pub fn copy_ui(&mut self, list: &ID3D12GraphicsCommandList, source: &ID3D12Resource, state: D3D12_RESOURCE_STATES) -> windows::core::Result<()> {
        // SAFETY: reads resource descriptors.
        let desc = unsafe { source.GetDesc() };
        let fits = self.ui.as_ref().is_some_and(|ui| {
            // SAFETY: as above.
            let have = unsafe { ui.GetDesc() };
            // A layer made elsewhere is replaced by the game's own (ours allows render targets).
            have.Width == desc.Width && have.Height == desc.Height && have.Flags == D3D12_RESOURCE_FLAG_NONE
        });
        if !fits {
            self.ui = None;
            let ui = d3d12::texture(&self.device, desc.Width as u32, desc.Height, DXGI_FORMAT_R8G8B8A8_UNORM, D3D12_RESOURCE_FLAG_NONE)?;
            // SAFETY: a view of our own texture into our own heap.
            unsafe { self.device.CreateShaderResourceView(&ui, Some(&srv_desc(DXGI_FORMAT_R8G8B8A8_UNORM)), self.heap.cpu(2)) };
            self.ui = Some(ui);
        }
        d3d12::copy_whole(list, self.ui.as_ref().expect("created above"), source, state);
        Ok(())
    }

    fn ensure_targets(&mut self, desc: &D3D12_RESOURCE_DESC) -> windows::core::Result<()> {
        if self.color.is_some() && desc.Width as u32 == self.width && desc.Height == self.height && desc.Format == self.format {
            return Ok(());
        }
        (self.color, self.keys, self.eyes, self.fallback) = (None, [None, None], [None, None], None);
        (self.width, self.height, self.format) = (desc.Width as u32, desc.Height, desc.Format);
        let color = d3d12::texture(&self.device, self.width, self.height, self.format, D3D12_RESOURCE_FLAG_NONE)?;
        // SAFETY: views of our own textures into our own heap.
        unsafe {
            self.device.CreateShaderResourceView(&color, Some(&srv_desc(self.format)), self.heap.cpu(0));
            for e in 0..2 {
                let keys = d3d12::texture(&self.device, self.width, self.height, DXGI_FORMAT_R32_UINT, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS)?;
                let eye = d3d12::texture(&self.device, self.width, self.height, DXGI_FORMAT_R8G8B8A8_UNORM, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS)?;
                self.device.CreateUnorderedAccessView(&keys, None, Some(&uav_desc(DXGI_FORMAT_R32_UINT)), self.heap.cpu(SRVS + 2 * e as u32));
                self.device.CreateUnorderedAccessView(&eye, None, Some(&uav_desc(DXGI_FORMAT_R8G8B8A8_UNORM)), self.heap.cpu(SRVS + 1 + 2 * e as u32));
                (self.keys[e], self.eyes[e]) = (Some(keys), Some(eye));
            }
        }
        self.color = Some(color);
        Ok(())
    }

    /// Copies the finished frame and makes both eyes on `queue`; the eye textures end in COMMON.
    /// `Ok(false)`: busy (it never waits) or no depth yet.
    pub fn run(&mut self, queue: &ID3D12CommandQueue, frame: &ID3D12Resource, frame_state: D3D12_RESOURCE_STATES, params: &Params) -> windows::core::Result<bool> {
        if !self.depth_ready {
            return Ok(false);
        }
        // SAFETY: reads a resource descriptor.
        let desc = unsafe { frame.GetDesc() };
        if desc.Width > 4096 {
            // The splat key holds a 12-bit column.
            return Err(windows::core::Error::from_hresult(windows::Win32::Graphics::Dxgi::DXGI_ERROR_UNSUPPORTED));
        }
        self.ensure_targets(&desc)?;
        let Some(list) = self.recorder.begin()? else { return Ok(false) };
        let (color, depth) = (self.color.clone().expect("ensured"), self.depth.clone().expect("depth ready"));
        const SRV: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
        let mut has_fallback = false;
        if let Some(image) = params.fallback {
            // SAFETY: reads a resource descriptor.
            let d = unsafe { image.GetDesc() };
            if d.Width == desc.Width && d.Height == desc.Height && d.Format == desc.Format {
                if self.fallback.is_none() {
                    let fallback = d3d12::texture(&self.device, self.width, self.height, self.format, D3D12_RESOURCE_FLAG_NONE)?;
                    // SAFETY: a view of our own texture into our own heap.
                    unsafe { self.device.CreateShaderResourceView(&fallback, Some(&srv_desc(self.format)), self.heap.cpu(3)) };
                    self.fallback = Some(fallback);
                }
                let fallback = self.fallback.as_ref().expect("created above");
                d3d12::copy_whole(&list, fallback, image, D3D12_RESOURCE_STATE_COMMON);
                barrier(&list, fallback, D3D12_RESOURCE_STATE_COMMON, SRV);
                has_fallback = true;
            }
        }
        d3d12::copy_whole(&list, &color, frame, frame_state);
        barrier(&list, &color, D3D12_RESOURCE_STATE_COMMON, SRV);
        barrier(&list, &depth, D3D12_RESOURCE_STATE_COMMON, SRV);
        let ui = self.ui.clone().filter(|ui| {
            // SAFETY: reads a resource descriptor.
            let d = unsafe { ui.GetDesc() };
            d.Width as u32 == self.width && d.Height == self.height
        });
        if let Some(ui) = &ui {
            barrier(&list, ui, D3D12_RESOURCE_STATE_COMMON, SRV);
        }
        let (gx, gy) = (self.width.div_ceil(8), self.height.div_ceil(8));
        let markers = self.markers.write(&params.markers);
        // SAFETY: records compute work on our own list with our own heap, root signature and
        // pipelines; every resource is live until the list has executed (the recorder's fence).
        unsafe {
            list.SetDescriptorHeaps(&[Some(self.heap.heap().clone())]);
            list.SetComputeRootSignature(&self.root);
            list.SetComputeRootDescriptorTable(1, self.heap.gpu(0));
            for e in 0..2 {
                let (keys, eye) = (self.keys[e].as_ref().expect("ensured"), self.eyes[e].as_ref().expect("ensured"));
                barrier(&list, keys, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
                barrier(&list, eye, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
                // The left eye sits left of the rendered camera, so the scene moves right in its image.
                let constants = self.constants(params, e, ui.is_some(), has_fallback, markers);
                list.SetComputeRoot32BitConstants(0, CONSTANTS, &constants as *const Constants as *const c_void, 0);
                list.SetComputeRootDescriptorTable(2, self.heap.gpu(SRVS + 2 * e as u32));
                list.SetPipelineState(&self.clear);
                list.Dispatch(gx, gy, 1);
                uav_barrier(&list, keys);
                list.SetPipelineState(&self.splat);
                list.Dispatch(gx, gy, 1);
                uav_barrier(&list, keys);
                list.SetPipelineState(&self.resolve);
                list.Dispatch(gx, gy, 1);
                barrier(&list, keys, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON);
                barrier(&list, eye, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON);
            }
        }
        barrier(&list, &color, SRV, D3D12_RESOURCE_STATE_COMMON);
        barrier(&list, &depth, SRV, D3D12_RESOURCE_STATE_COMMON);
        if let Some(ui) = &ui {
            barrier(&list, ui, SRV, D3D12_RESOURCE_STATE_COMMON);
        }
        if has_fallback {
            barrier(&list, self.fallback.as_ref().expect("copied above"), SRV, D3D12_RESOURCE_STATE_COMMON);
        }
        self.recorder.submit(queue)?;
        Ok(true)
    }

    /// The shader's constants for eye `e` (0 left: the scene moves right in its image).
    fn constants(&self, params: &Params, e: usize, has_ui: bool, has_fallback: bool, markers: (u32, u32)) -> Constants {
        build_constants((self.width, self.height), (self.depth_width, self.depth_height), params, e, has_ui, has_fallback, markers)
    }


    /// Lays the UI layer over one rendered eye's `frame` (no warp: alternate-eye and same-frame
    /// stereo) into the eye texture `eye` (0 left, 1 right), which ends in COMMON. `Ok(false)`:
    /// busy (it never waits).
    pub fn overlay(&mut self, queue: &ID3D12CommandQueue, frame: &ID3D12Resource, frame_state: D3D12_RESOURCE_STATES, params: &Params, eye: usize) -> windows::core::Result<bool> {
        // SAFETY: reads a resource descriptor.
        let desc = unsafe { frame.GetDesc() };
        self.ensure_targets(&desc)?;
        let Some(list) = self.recorder.begin()? else { return Ok(false) };
        let color = self.color.clone().expect("ensured");
        const SRV: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
        d3d12::copy_whole(&list, &color, frame, frame_state);
        barrier(&list, &color, D3D12_RESOURCE_STATE_COMMON, SRV);
        let ui = self.ui.clone().filter(|ui| {
            // SAFETY: reads a resource descriptor.
            let d = unsafe { ui.GetDesc() };
            d.Width as u32 == self.width && d.Height == self.height
        });
        if let Some(ui) = &ui {
            barrier(&list, ui, D3D12_RESOURCE_STATE_COMMON, SRV);
        }
        let out = self.eyes[eye].clone().expect("ensured");
        let markers = self.markers.write(&params.markers);
        let constants = self.constants(params, eye, ui.is_some(), false, markers);
        // SAFETY: as in `run`.
        unsafe {
            list.SetDescriptorHeaps(&[Some(self.heap.heap().clone())]);
            list.SetComputeRootSignature(&self.root);
            list.SetComputeRootDescriptorTable(1, self.heap.gpu(0));
            list.SetComputeRootDescriptorTable(2, self.heap.gpu(SRVS + 2 * eye as u32));
            list.SetComputeRoot32BitConstants(0, CONSTANTS, &constants as *const Constants as *const c_void, 0);
            barrier(&list, &out, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            list.SetPipelineState(&self.overlay);
            list.Dispatch(self.width.div_ceil(8), self.height.div_ceil(8), 1);
            barrier(&list, &out, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON);
        }
        barrier(&list, &color, SRV, D3D12_RESOURCE_STATE_COMMON);
        if let Some(ui) = &ui {
            barrier(&list, ui, SRV, D3D12_RESOURCE_STATE_COMMON);
        }
        self.recorder.submit(queue)?;
        Ok(true)
    }

    /// Waits (at most five seconds) for the GPU before the resources may be released.
    pub fn idle(&self) -> bool {
        self.recorder.idle(5000)
    }
}

/// The shader's root constants (32-bit words).
const CONSTANTS: u32 = (std::mem::size_of::<Constants>() / 4) as u32;
const _: () = assert!(std::mem::size_of::<Constants>() == 44 * 4);

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;

    #[test]
    fn shaders_compile_and_the_warp_initialises() {
        for entry in [s!("Clear"), s!("Splat"), s!("Resolve")] {
            compute::compile(&source(), "warp12.hlsl", entry).unwrap();
        }
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: creates a device on the default adapter.
        unsafe { D3D12CreateDevice(None::<&windows::core::IUnknown>, D3D_FEATURE_LEVEL_11_0, &mut device) }.unwrap();
        let warp = DepthWarp::new(&device.unwrap()).unwrap();
        assert!(warp.eye(0).is_none() && warp.ui().is_none());
    }
}

/// The shader's constants for a `size` frame (`depth_size` the depth's) and eye `e` (0 left: the
/// scene moves right in its image).
fn build_constants(size: (u32, u32), depth_size: (u32, u32), params: &Params, e: usize, has_ui: bool, has_fallback: bool, markers: (u32, u32)) -> Constants {
    let shown: Vec<[f32; 4]> = params.pieces.iter().copied().filter(|r| r[2] > 0.0 && r[3] > 0.0).collect();
    let mut piece_rects = [[0.0; 4]; 5];
    for (slot, rect) in piece_rects.iter_mut().zip(&shown) {
        *slot = *rect;
    }
    Constants {
        width: size.0,
        height: size.1,
        depth_width: depth_size.0,
        depth_height: depth_size.1,
        scale: params.scale,
        direction: if e == 0 { 1.0 } else { -1.0 },
        max_shift: params.max_shift,
        has_ui: match has_ui {
            false => 0,
            true if params.ui_out_of_frame => 2,
            true => 1,
        },
        hud_rect: params.hud[e],
        marker_count: markers.1 as f32,
        aim_x: params.aim[0],
        aim_y: params.aim[1],
        cross_radius: params.cross_radius,
        hud_from_frame: if params.hud_from_frame { if has_fallback { 2.0 } else { 1.0 } } else { 0.0 },
        grid_offset_x: params.grid[1],
        grid_scale_x: params.grid[0],
        grid_scale_y: params.grid[2],
        grid_offset_y: params.grid[3],
        pieces: shown.len() as f32,
        marker_base: markers.0 as f32,
        pad: 0.0,
        piece_rects,
    }
}

/// The UI layer laid over a frame, recorded on a caller's command list (frame generation: its
/// real and generated frames): the warp's `Overlay` pass with a ring of descriptor sets.
pub struct HudOverlay {
    device: ID3D12Device,
    root: ID3D12RootSignature,
    pipeline: ID3D12PipelineState,
    heap: DescriptorHeap,
    next: u32,
    markers: MarkerRing,
}

/// Descriptors per set: the frame, the depth (unused), the UI layer, the fallback (unused), the
/// markers; the keys (unused) and the output.
const OVERLAY_SET: u32 = SRVS + 2;
const OVERLAY_SETS: u32 = 8;

// SAFETY: D3D12 objects are free-threaded; the overlay is used under its owner's lock.
unsafe impl Send for HudOverlay {}

impl HudOverlay {
    pub fn new(device: &ID3D12Device) -> Result<Self, String> {
        let root = compute::root_signature(device, (size_of::<Constants>() / 4) as u32, &[Table { srvs: SRVS, uavs: 0 }, Table { srvs: 0, uavs: 2 }])?;
        let pipeline = compute::shader_pipeline(device, &root, &source(), "warp12.hlsl", s!("Overlay"))?;
        let heap = DescriptorHeap::new(device, OVERLAY_SET * OVERLAY_SETS)?;
        let markers = MarkerRing::new(device)?;
        Ok(Self { device: device.clone(), root, pipeline, heap, next: 0, markers })
    }

    /// Records on `list`: `frame` (any RGBA8 format, in COMMON) with the UI `layer` (RGBA8, in
    /// COMMON) laid over as `params` say for eye `eye` (0 left), into `out` (RGBA8 UNORM, allows
    /// unordered access, in COMMON; the frame's size). Everything ends in COMMON.
    pub fn lay(&mut self, list: &ID3D12GraphicsCommandList, frame: &ID3D12Resource, layer: &ID3D12Resource, out: &ID3D12Resource, params: &Params, eye: usize) {
        // SAFETY: reads descriptors.
        let (frame_desc, layer_desc) = unsafe { (frame.GetDesc(), layer.GetDesc()) };
        let size = (frame_desc.Width as u32, frame_desc.Height);
        let has_ui = layer_desc.Width == frame_desc.Width && layer_desc.Height == frame_desc.Height;
        let base = self.next * OVERLAY_SET;
        self.next = (self.next + 1) % OVERLAY_SETS;
        // SAFETY: views of live textures (and null views) into our own heap.
        unsafe {
            self.device.CreateShaderResourceView(frame, Some(&srv_desc(frame_desc.Format)), self.heap.cpu(base));
            self.device.CreateShaderResourceView(None, Some(&srv_desc(DXGI_FORMAT_R32_FLOAT)), self.heap.cpu(base + 1));
            self.device.CreateShaderResourceView(layer, Some(&srv_desc(DXGI_FORMAT_R8G8B8A8_UNORM)), self.heap.cpu(base + 2));
            self.device.CreateShaderResourceView(None, Some(&srv_desc(DXGI_FORMAT_R8G8B8A8_UNORM)), self.heap.cpu(base + 3));
            self.device.CreateUnorderedAccessView(None, None, Some(&uav_desc(DXGI_FORMAT_R32_UINT)), self.heap.cpu(base + SRVS));
            self.device.CreateUnorderedAccessView(out, None, Some(&uav_desc(DXGI_FORMAT_R8G8B8A8_UNORM)), self.heap.cpu(base + SRVS + 1));
        }
        self.markers.view(&self.device, self.heap.cpu(base + 4));
        let markers = self.markers.write(&params.markers);
        const SRV: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
        barrier(list, frame, D3D12_RESOURCE_STATE_COMMON, SRV);
        if has_ui {
            barrier(list, layer, D3D12_RESOURCE_STATE_COMMON, SRV);
        }
        barrier(list, out, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        let constants = build_constants(size, (0, 0), params, eye, has_ui, false, markers);
        // SAFETY: records compute work with our own heap, root signature and pipeline.
        unsafe {
            list.SetDescriptorHeaps(&[Some(self.heap.heap().clone())]);
            list.SetComputeRootSignature(&self.root);
            list.SetPipelineState(&self.pipeline);
            list.SetComputeRoot32BitConstants(0, CONSTANTS, &constants as *const Constants as *const c_void, 0);
            list.SetComputeRootDescriptorTable(1, self.heap.gpu(base));
            list.SetComputeRootDescriptorTable(2, self.heap.gpu(base + SRVS));
            list.Dispatch(size.0.div_ceil(8), size.1.div_ceil(8), 1);
        }
        barrier(list, out, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON);
        barrier(list, frame, SRV, D3D12_RESOURCE_STATE_COMMON);
        if has_ui {
            barrier(list, layer, SRV, D3D12_RESOURCE_STATE_COMMON);
        }
    }
}
