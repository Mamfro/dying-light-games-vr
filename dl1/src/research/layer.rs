//! `probe_layer` (with FSR): what reaches the headset, piece by piece. Every [`EVERY_MS`] (up to
//! [`DUMPS`] times) one frame is written beside the log as PNG: the back buffer at present (what
//! the game drew, without the HUD draws routed into the layer), the eye image before the HUD goes
//! over it, and the HUD layer as drawn onto black. When a menu shows shrunk or missing in the
//! headset, these show whether the game drew it wrong or the layer placed it wrong.

use super::options;
use monaka_channel::d3d;
use monaka_producer::log;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};

const EVERY_MS: u64 = 1000;
const DUMPS: u64 = 16;

static LAST: AtomicU64 = AtomicU64::new(0);
static COUNT: AtomicU64 = AtomicU64::new(0);

/// A frame about to go to the headset (`kind`: "eye" or "flat"): dumped when one is due. Reads
/// back on the render thread (stalls; probing only).
pub fn frame(device: &ID3D11Device, context: &ID3D11DeviceContext, back: &ID3D11Texture2D, eye: &ID3D11Texture2D, layer: Option<ID3D11Texture2D>, kind: &str) {
    if !options().layer {
        return;
    }
    let now = monaka_channel::tick();
    if now.saturating_sub(LAST.load(Relaxed)) < EVERY_MS || COUNT.load(Relaxed) >= DUMPS {
        return;
    }
    LAST.store(now, Relaxed);
    let n = COUNT.fetch_add(1, Relaxed);
    let mut written = Vec::new();
    for (name, texture) in [("back", Some(back.clone())), ("eye", Some(eye.clone())), ("layer", layer)] {
        let Some(texture) = texture else { continue };
        let path = super::dir().join(format!("layer-{n:02}-{kind}-{name}.png"));
        match d3d::read_image(device, context, &texture) {
            Ok((width, height, mut rgba)) => {
                // Opaque for viewing (the layer's alpha is its coverage; its colour is on black).
                rgba.chunks_exact_mut(4).for_each(|p| p[3] = 255);
                match std::fs::write(&path, monaka_core::png::rgba(width, height, &rgba)) {
                    Ok(()) => written.push(format!("{name} {width}x{height}")),
                    Err(e) => log!("probe_layer: {}: {e}", path.display()),
                }
            }
            Err(e) => log!("probe_layer: {name} unreadable: {e}"),
        }
    }
    log!("probe_layer: dump {n} ({kind} frame): {}", written.join(", "));
}
