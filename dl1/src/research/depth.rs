//! `probe_depth`, groundwork for depth-based stereo in Dying Light 1. The warp itself is shared
//! (`monaka_warp`, with the math in [`monaka_core::depth`]); what DL1 has to supply is what
//! Streamline handed DL2:
//!
//! - the scene's depth buffer at the end of the 3D pass: this probe logs which depth textures
//!   were bound last before the scene reached the back buffer (the unblended three-vertex copy);
//! - the near distance and projection the depth was rendered with (the render camera state:
//!   `STATE_PROJECTION`, `STATE_NEAR`), for [`monaka_core::depth::DepthMapping`];
//! - a UI layer: the HUD draws after the scene copy already pass through our hooks, so they can be
//!   drawn into a private transparent target instead of the back buffer (not built yet; the
//!   blend states must give premultiplied colour there, to be checked on a capture).
//!
//! With those, one render per frame gives both eyes from the same moment (alternate-eye shows
//! them a frame apart) at one pair per game frame.

use monaka_producer::log;
use std::sync::Mutex;
use windows::Win32::Graphics::Direct3D11::{ID3D11DepthStencilView, ID3D11Texture2D};
use windows::core::Interface;

#[derive(Clone, Copy)]
struct Candidate {
    texture: usize,
    width: u32,
    height: u32,
    format: i32,
    binds: u64,
    last_before_scene: u64,
}

struct Probe {
    candidates: Vec<Candidate>,
    last: Option<usize>,
}

static PROBE: Mutex<Probe> = Mutex::new(Probe { candidates: Vec::new(), last: None });

/// A depth view the game bound on the immediate context (or null).
pub fn bound(view: *mut core::ffi::c_void) {
    if !super::options().depth || view.is_null() {
        return;
    }
    // SAFETY: the game passes a live depth-stencil view (or null).
    let Some(view) = (unsafe { ID3D11DepthStencilView::from_raw_borrowed(&view) }) else { return };
    // SAFETY: COM calls on a live view and its resource.
    let Some(texture) = (unsafe { view.GetResource() }).ok().and_then(|r| r.cast::<ID3D11Texture2D>().ok()) else { return };
    let key = texture.as_raw() as usize;
    let Ok(mut probe) = PROBE.lock() else { return };
    probe.last = Some(key);
    if let Some(known) = probe.candidates.iter_mut().find(|c| c.texture == key) {
        known.binds += 1;
    } else if probe.candidates.len() < 64 {
        let desc = monaka_channel::d3d::texture_desc(&texture);
        probe.candidates.push(Candidate {
            texture: key,
            width: desc.Width,
            height: desc.Height,
            format: desc.Format.0,
            binds: 1,
            last_before_scene: 0,
        });
    }
}

/// The scene just reached the back buffer: the depth bound last is the scene's candidate.
pub fn scene_copied() {
    if !super::options().depth {
        return;
    }
    let Ok(mut probe) = PROBE.lock() else { return };
    if let Some(last) = probe.last.take()
        && let Some(candidate) = probe.candidates.iter_mut().find(|c| c.texture == last)
    {
        candidate.last_before_scene += 1;
    }
}

/// Logs the candidates, most often last-before-scene first (end of a run).
pub fn report() {
    let Ok(mut probe) = PROBE.lock() else { return };
    if probe.candidates.is_empty() {
        return;
    }
    probe.candidates.sort_by(|a, b| b.last_before_scene.cmp(&a.last_before_scene).then(b.binds.cmp(&a.binds)));
    for c in probe.candidates.iter().take(12) {
        log!(
            "depth candidate {:#x}: {}x{} DXGI format {} binds={} last_before_scene={}",
            c.texture,
            c.width,
            c.height,
            c.format,
            c.binds,
            c.last_before_scene
        );
    }
}
