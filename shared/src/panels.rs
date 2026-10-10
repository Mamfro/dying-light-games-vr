//! The dynamic HUD of the D3D12 Chrome Engine games (`world_hud`): HUD pieces out of a UI layer
//! (the game's own, tagged for frame generation, or [`crate::hudlayer`]'s), onto the hands. The game hands its HUD over as a layer of its own (tagged for DLSS
//! frame generation; `warp12` lays it over each eye). Each piece is a widget of the engine's gui
//! tree (the game's [`crate::gui::Piece`] table), whose box on screen the game thread reads every frame
//! ([`crate::gui::layout`]). At each depth-stereo present, before the warp, each shown piece's
//! rectangle of the layer is scaled into its panel (a fixed-size premultiplied RGBA texture) and
//! published on its own fence channel, `<channel>-panel-<piece>`, for the viewer to place
//! (`apps/viewer/src/panel.rs`, the same pieces as Dying Light 1's); the warp is told the
//! rectangles and leaves them out of the HUD it lays over the eyes (the game draws its HUD into
//! the frame too, and the warp takes it out of the world from the layer, so the layer must stay
//! whole: a piece blanked in the layer would stay in the frame). A piece's panel shows its
//! widget's box as the game laid it out, fitted to the panel's shape with a margin
//! ([`monaka_core::hud::fit`]). The shaders are `panels12.hlsl`.

use crate::gui::{self, Layout};
use monaka_channel::ChannelName;
use monaka_core::hud::LAYOUT_FRESH_MS;
use monaka_channel::d3d12::compute::{self, DescriptorHeap, Table, srv_desc, uav_desc};
use monaka_channel::d3d12::{self, Recorder, barrier};
use monaka_channel::fence::FenceProducer;
use monaka_producer::log;
use std::ffi::c_void;
use std::sync::Mutex;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM;
use windows::core::s;

const SOURCE: &str = include_str!("panels12.hlsl");
/// A piece's box grown by this share of its larger side on each side for its panel (small: the
/// compass bar is twenty times as wide as tall, and the immunity widget sits right under it).
const MARGIN: f32 = 0.02;

/// The shader's constant buffer (8 root constants).
#[repr(C)]
#[derive(Clone, Copy)]
struct Constants {
    out_width: u32,
    out_height: u32,
    ui_width: u32,
    ui_height: u32,
    source: [f32; 4],
}

struct Piece {
    name: &'static str,
    size: (u32, u32),
    texture: ID3D12Resource,
    producer: Option<FenceProducer>,
    /// Its widget's box (layer pixels) this present, when shown.
    found: Option<[f32; 4]>,
    /// Published shown the frame before.
    published_shown: bool,
    failures: u64,
}

struct Panels {
    device: ID3D12Device,
    root: ID3D12RootSignature,
    crop: ID3D12PipelineState,
    clear: ID3D12PipelineState,
    heap: DescriptorHeap,
    recorder: Recorder,
    pieces: Vec<Piece>,
    /// The UI layer the heap's views were made for.
    viewed: Option<ID3D12Resource>,
    published: u64,
}

static PANELS: Mutex<Option<Panels>> = Mutex::new(None);
static FAILED: Mutex<bool> = Mutex::new(false);

impl Panels {
    fn new(device: &ID3D12Device) -> Result<Self, String> {
        // Constants, then the UI layer (t0), then one panel (u0).
        let root = compute::root_signature(device, (size_of::<Constants>() / 4) as u32, &[Table { srvs: 1, uavs: 0 }, Table { srvs: 0, uavs: 1 }])?;
        let pipeline = |entry| compute::shader_pipeline(device, &root, SOURCE, "panels12.hlsl", entry);
        let (crop, clear) = (pipeline(s!("Crop"))?, pipeline(s!("Clear"))?);
        let wanted = gui::pieces();
        let heap = DescriptorHeap::new(device, 1 + wanted.len().max(1) as u32)?;
        let recorder = Recorder::new(device, 4).map_err(|e| format!("command lists: {e}"))?;
        let mut pieces = Vec::new();
        for piece in wanted {
            let (name, size) = (piece.name, piece.size);
            let texture = d3d12::texture(device, size.0, size.1, DXGI_FORMAT_R8G8B8A8_UNORM, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS).map_err(|e| format!("panel {name}: {e}"))?;
            pieces.push(Piece { name, size, texture, producer: None, found: None, published_shown: false, failures: 0 });
        }
        Ok(Self { device: device.clone(), root, crop, clear, heap, recorder, pieces, viewed: None, published: 0 })
    }

    /// Views: the layer's SRV at 0, then each piece's panel's UAV.
    fn view(&mut self, ui: &ID3D12Resource) {
        if self.viewed.as_ref() == Some(ui) {
            return;
        }
        let rgba = DXGI_FORMAT_R8G8B8A8_UNORM;
        // SAFETY: views of live textures into our own heap.
        unsafe {
            self.device.CreateShaderResourceView(ui, Some(&srv_desc(rgba)), self.heap.cpu(0));
            for (i, piece) in self.pieces.iter().enumerate() {
                self.device.CreateUnorderedAccessView(&piece.texture, None, Some(&uav_desc(rgba)), self.heap.cpu(1 + i as u32));
            }
        }
        self.viewed = Some(ui.clone());
    }

    /// Records this present's panels from `ui` (in COMMON) for the pieces `layout` shows: which
    /// pieces to publish (shown ones and ones just cleared), with each shown piece's rectangle of
    /// the layer (x, y, width, height).
    fn record(&mut self, ui: &ID3D12Resource, ui_size: (u32, u32), layout: Option<&Layout>) -> windows::core::Result<Option<(Vec<usize>, Vec<[f32; 4]>)>> {
        let Some(list) = self.recorder.begin()? else { return Ok(None) };
        self.view(ui);
        let mut rects: Vec<(usize, [f32; 4])> = Vec::new();
        let mut cleared: Vec<usize> = Vec::new();
        for (i, piece) in self.pieces.iter_mut().enumerate() {
            match layout.and_then(|l| l.pieces[i]) {
                Some(b) => {
                    // The widget's own box, as laid out: stable, so it is taken as it is. Its
                    // pixels are the frame's: in a square headset eye the 16:9 layout sits
                    // letterboxed and the world matrices carry that offset, so the layout's size
                    // is not a scale to apply.
                    piece.found = Some(b);
                    rects.push((i, monaka_core::hud::fit(b, MARGIN, piece.size)));
                }
                None => {
                    piece.found = None;
                    if piece.published_shown {
                        cleared.push(i);
                    }
                }
            }
        }
        const SRV: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
        let constants = |piece: &Piece, source: [f32; 4]| Constants { out_width: piece.size.0, out_height: piece.size.1, ui_width: ui_size.0, ui_height: ui_size.1, source };
        let touched: Vec<usize> = rects.iter().map(|(i, _)| *i).chain(cleared.iter().copied()).collect();
        let shown: Vec<[f32; 4]> = rects.iter().map(|(_, r)| *r).collect();
        if touched.is_empty() {
            return Ok(Some((touched, shown)));
        }
        // SAFETY: records compute work on our own list with our own heap, root signature and
        // pipelines; every resource is live until the list has executed (the recorder's fence).
        unsafe {
            list.SetDescriptorHeaps(&[Some(self.heap.heap().clone())]);
            list.SetComputeRootSignature(&self.root);
            list.SetComputeRootDescriptorTable(1, self.heap.gpu(0));
            for &i in &touched {
                barrier(&list, &self.pieces[i].texture, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            }
            barrier(&list, ui, D3D12_RESOURCE_STATE_COMMON, SRV);
            list.SetPipelineState(&self.crop);
            for &(i, rect) in &rects {
                let piece = &self.pieces[i];
                let c = constants(piece, rect);
                list.SetComputeRoot32BitConstants(0, 8, &c as *const Constants as *const c_void, 0);
                list.SetComputeRootDescriptorTable(2, self.heap.gpu(1 + i as u32));
                list.Dispatch(piece.size.0.div_ceil(8), piece.size.1.div_ceil(8), 1);
            }
            barrier(&list, ui, SRV, D3D12_RESOURCE_STATE_COMMON);
            list.SetPipelineState(&self.clear);
            for &i in &cleared {
                let piece = &self.pieces[i];
                let c = constants(piece, [0.0; 4]);
                list.SetComputeRoot32BitConstants(0, 8, &c as *const Constants as *const c_void, 0);
                list.SetComputeRootDescriptorTable(2, self.heap.gpu(1 + i as u32));
                list.Dispatch(piece.size.0.div_ceil(8), piece.size.1.div_ceil(8), 1);
            }
            for &i in &touched {
                barrier(&list, &self.pieces[i].texture, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON);
            }
        }
        Ok(Some((touched, shown)))
    }
}

/// At a depth-stereo present, before the warp: the pieces' panels from the UI layer `ui` (in
/// COMMON), published on `channel`'s panel channels. Returns the shown pieces' rectangles of the
/// layer (x, y, width, height), for the warp to leave out of the eyes.
pub fn publish(queue: &ID3D12CommandQueue, ui: &ID3D12Resource, channel: &ChannelName, tick: u64) -> Vec<[f32; 4]> {
    if *FAILED.lock().unwrap_or_else(|e| e.into_inner()) {
        return Vec::new();
    }
    let mut panels = PANELS.lock().unwrap_or_else(|e| e.into_inner());
    if panels.is_none() {
        match d3d12::device_of(queue).map_err(|e| e.to_string()).and_then(|device| Panels::new(&device)) {
            Ok(made) => {
                log!("dynamic HUD: panels ready ({} pieces)", made.pieces.len());
                *panels = Some(made);
            }
            Err(why) => {
                log!("dynamic HUD off: {why}");
                *FAILED.lock().unwrap_or_else(|e| e.into_inner()) = true;
                return Vec::new();
            }
        }
    }
    let Some(panels) = panels.as_mut() else { return Vec::new() };
    // SAFETY: reads a resource descriptor.
    let desc = unsafe { ui.GetDesc() };
    let ui_size = (desc.Width as u32, desc.Height);
    let layout = gui::layout().filter(|l| tick.saturating_sub(l.taken) <= LAYOUT_FRESH_MS);
    let (publish, shown) = match panels.record(ui, ui_size, layout.as_ref()) {
        Ok(Some(recorded)) => recorded,
        Ok(None) => return Vec::new(),
        Err(e) => {
            monaka_producer::log_first!(3, "dynamic HUD: panels not recorded: {e}");
            return Vec::new();
        }
    };
    if let Err(e) = panels.recorder.submit(queue) {
        monaka_producer::log_first!(3, "dynamic HUD: panels not submitted: {e}");
        return Vec::new();
    }
    if publish.is_empty() {
        return shown;
    }
    let device = panels.device.clone();
    for i in publish {
        let shown = panels.pieces[i].found.is_some();
        let piece = &mut panels.pieces[i];
        if piece.producer.is_none() {
            let name = channel.panel(piece.name);
            match name.ok_or_else(|| "bad channel name".to_owned()).and_then(|name| FenceProducer::create(&name, &device, queue, piece.size.0, piece.size.1, DXGI_FORMAT_R8G8B8A8_UNORM).map_err(|e| e.to_string())) {
                Ok(producer) => {
                    log!("dynamic HUD: panel {} published on its channel ({}x{})", piece.name, piece.size.0, piece.size.1);
                    piece.producer = Some(producer);
                }
                Err(why) => {
                    piece.failures += 1;
                    if piece.failures <= 3 {
                        log!("dynamic HUD: panel {} has no channel: {why}", piece.name);
                    }
                    continue;
                }
            }
        }
        let Some(producer) = piece.producer.as_mut() else { continue };
        match producer.publish((&piece.texture, D3D12_RESOURCE_STATE_COMMON), (&piece.texture, D3D12_RESOURCE_STATE_COMMON), tick) {
            Ok(true) => {
                piece.published_shown = shown;
                panels.published += 1;
            }
            Ok(false) => {}
            Err(e) => {
                piece.failures += 1;
                if piece.failures <= 3 {
                    log!("dynamic HUD: panel {} publish failed: {e}", piece.name);
                }
            }
        }
    }
    shown
}

/// The end of a run: the channels go away with the producers, once the GPU is done with the panels.
pub fn close() {
    let mut panels = PANELS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(made) = panels.take() {
        made.recorder.idle(2000);
        log!("dynamic HUD: {} panels published", made.published);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;

    #[test]
    fn shaders_compile_and_the_panels_initialise() {
        for entry in [s!("Crop"), s!("Clear")] {
            compute::compile(SOURCE, "panels12.hlsl", entry).unwrap();
        }
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: creates a device on the default adapter.
        unsafe { D3D12CreateDevice(None::<&windows::core::IUnknown>, D3D_FEATURE_LEVEL_11_0, &mut device) }.unwrap();
        let panels = Panels::new(&device.unwrap()).unwrap();
        assert_eq!(panels.pieces.len(), gui::pieces().len());
    }
}
