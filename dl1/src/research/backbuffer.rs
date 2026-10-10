//! `probe_backbuffer`: how a frame reaches the back buffer. The HUD routing ([`crate::hud::draws`])
//! takes the frame's first unblended three-vertex draw onto the back buffer as the finished 3D
//! image and every later back-buffer draw as HUD; in a frame where that copy is not seen, the HUD
//! draws are not routed and the HUD stays in each eye's image. For a few frames this
//! logs every back-buffer binding and every draw onto it (vertices, blending, viewport, the pixel
//! shader and what it reads), and over the run how many frames had that copy.

use super::options;
use monaka_channel::d3d;
use monaka_producer::log;
use std::sync::Mutex;
use windows::Win32::Graphics::Direct3D11::*;
use windows::core::Interface;

/// The frames (presents from attachment) logged draw by draw: two in a row, so both eyes.
const LOGGED: [u64; 4] = [600, 601, 2400, 2401];

struct Counts {
    frame: u64,
    /// This frame: draws onto the back buffer before and after the 3D image's copy, and whether the
    /// copy was seen.
    before: u64,
    after: u64,
    copied: bool,
    frames: u64,
    frames_copied: u64,
    draws_before: u64,
    draws_after: u64,
    /// Frames with back-buffer draws but no copy seen.
    frames_uncopied_drawn: u64,
}

static COUNTS: Mutex<Counts> =
    Mutex::new(Counts { frame: u64::MAX, before: 0, after: 0, copied: false, frames: 0, frames_copied: 0, draws_before: 0, draws_after: 0, frames_uncopied_drawn: 0 });

fn enabled() -> bool {
    options().backbuffer
}

/// Present `frame` is being drawn: the last frame's counts are added up.
fn at_frame(counts: &mut Counts, frame: u64) {
    if counts.frame == frame {
        return;
    }
    if counts.frame != u64::MAX {
        counts.frames += 1;
        counts.frames_copied += counts.copied as u64;
        counts.draws_before += counts.before;
        counts.draws_after += counts.after;
        counts.frames_uncopied_drawn += (!counts.copied && counts.before > 0) as u64;
    }
    counts.frame = frame;
    counts.before = 0;
    counts.after = 0;
    counts.copied = false;
}

/// The game bound `count` targets: `on` when the first is the back buffer.
pub fn bound(context: &ID3D11DeviceContext, on: bool, count: u32, depth: bool) {
    if !enabled() {
        return;
    }
    let Some(frame) = crate::view::stereo::drawing_present() else { return };
    if !LOGGED.contains(&frame) {
        return;
    }
    if on {
        log!("backbuffer frame {frame}: bound ({count} targets, depth {depth})");
    } else {
        let mut target = [None];
        // SAFETY: reads the bound render target into a local.
        unsafe { context.OMGetRenderTargets(Some(&mut target), None) };
        let what = target[0].as_ref().and_then(|v| unsafe { v.GetResource() }.ok()).and_then(|r| r.cast::<ID3D11Texture2D>().ok()).map(|t| {
            let d = d3d::texture_desc(&t);
            format!("{}x{} format {}", d.Width, d.Height, d.Format.0)
        });
        log!("backbuffer frame {frame}: left for {count} targets ({})", what.unwrap_or_else(|| "none".into()));
    }
}

/// A draw of `vertices` onto the back buffer; `copied`: the 3D image's copy was seen before it.
pub fn draw(context: &ID3D11DeviceContext, vertices: u32, copied: bool) {
    if !enabled() {
        return;
    }
    let Some(frame) = crate::view::stereo::drawing_present() else { return };
    let index = {
        let Ok(mut counts) = COUNTS.lock() else { return };
        at_frame(&mut counts, frame);
        counts.copied |= copied;
        if copied {
            counts.after += 1;
        } else {
            counts.before += 1;
        }
        counts.before + counts.after - 1
    };
    if copied && let Some(size) = crate::hud::draws::backbuffer_size() {
        note_odd_sizes(context, frame, vertices, size);
    }
    if LOGGED.contains(&frame) && index < 400 {
        log_draw(context, frame, index, vertices, copied);
    }
}

/// A shader view's texture as `WxH fFORMAT` (`rt` when it is also a render target).
fn describe(view: &ID3D11ShaderResourceView) -> String {
    // SAFETY: COM call on a live view.
    let texture = unsafe { view.GetResource() }.ok().and_then(|r| r.cast::<ID3D11Texture2D>().ok());
    texture.map_or_else(
        || "not 2D".into(),
        |t| {
            let d = d3d::texture_desc(&t);
            let rt = if d.BindFlags & D3D11_BIND_RENDER_TARGET.0 as u32 != 0 { " rt" } else { "" };
            format!("{}x{} f{}{rt}", d.Width, d.Height, d.Format.0)
        },
    )
}

/// Texture sizes HUD draws read that are not the back buffer's, each logged once (up to 24).
static ODD_SIZES: Mutex<Vec<(u32, u32, bool)>> = Mutex::new(Vec::new());

/// After the 3D image's copy, any frame: a render target the draw reads at a size other than the
/// back buffer's `size` is logged the first time (for a menu that shows shrunk into a corner).
fn note_odd_sizes(context: &ID3D11DeviceContext, frame: u64, vertices: u32, size: (u32, u32)) {
    let mut views: [Option<ID3D11ShaderResourceView>; 8] = Default::default();
    // SAFETY: reads the game's bound shader views into locals.
    unsafe { context.PSGetShaderResources(0, Some(&mut views)) };
    for (slot, view) in views.iter().enumerate() {
        // SAFETY: COM call on a live view.
        let Some(t) = view.as_ref().and_then(|v| unsafe { v.GetResource() }.ok()).and_then(|r| r.cast::<ID3D11Texture2D>().ok()) else { continue };
        let d = d3d::texture_desc(&t);
        let rt = d.BindFlags & D3D11_BIND_RENDER_TARGET.0 as u32 != 0;
        if (d.Width, d.Height) == size || (!rt && d.Width.max(d.Height) < 1024) {
            continue;
        }
        let Ok(mut seen) = ODD_SIZES.lock() else { return };
        if seen.len() >= 24 || seen.contains(&(d.Width, d.Height, rt)) {
            continue;
        }
        seen.push((d.Width, d.Height, rt));
        drop(seen);
        let mut count = 1u32;
        let mut viewport = [D3D11_VIEWPORT::default()];
        // SAFETY: reads one viewport into a local.
        unsafe { context.RSGetViewports(&mut count, Some(viewport.as_mut_ptr())) };
        let v = viewport[0];
        log!(
            "backbuffer frame {frame}: a HUD draw ({vertices} vertices, viewport {:.0},{:.0} {:.0}x{:.0}) reads t{slot} {}x{} f{}{}",
            v.TopLeftX,
            v.TopLeftY,
            v.Width,
            v.Height,
            d.Width,
            d.Height,
            d.Format.0,
            if rt { " (a render target)" } else { "" }
        );
    }
}

fn log_draw(context: &ID3D11DeviceContext, frame: u64, index: u64, vertices: u32, copied: bool) {
    let mut blend: Option<ID3D11BlendState> = None;
    let mut blend_desc = D3D11_BLEND_DESC::default();
    let mut viewport = [D3D11_VIEWPORT::default()];
    let mut count = 1u32;
    let mut shader: Option<ID3D11PixelShader> = None;
    let mut views: [Option<ID3D11ShaderResourceView>; 8] = Default::default();
    // SAFETY: reads the game's bound state into locals on its own context and thread.
    unsafe {
        context.OMGetBlendState(Some(&mut blend), None, None);
        if let Some(blend) = &blend {
            blend.GetDesc(&mut blend_desc);
        }
        context.RSGetViewports(&mut count, Some(viewport.as_mut_ptr()));
        context.PSGetShader(&mut shader, None, None);
        context.PSGetShaderResources(0, Some(&mut views));
    }
    let reads: Vec<String> = views.iter().enumerate().filter_map(|(slot, view)| Some(format!("t{slot} {}", describe(view.as_ref()?)))).collect();
    let v = viewport[0];
    log!(
        "backbuffer frame {frame} draw {index}: {vertices} vertices, blend {}, viewport {:.0},{:.0} {:.0}x{:.0}, ps {:#x}, reads [{}]{}",
        blend_desc.RenderTarget[0].BlendEnable.as_bool(),
        v.TopLeftX,
        v.TopLeftY,
        v.Width,
        v.Height,
        shader.as_ref().map_or(0, |s| s.as_raw() as usize),
        reads.join(", "),
        if copied { "" } else { " (before the 3D image's copy)" }
    );
}

/// End of a run: how many frames had the 3D image's copy.
pub fn report() {
    if !enabled() {
        return;
    }
    let Ok(mut counts) = COUNTS.lock() else { return };
    at_frame(&mut counts, u64::MAX - 1);
    let frames = counts.frames.max(1);
    log!(
        "probe_backbuffer: {} frames, the 3D image's copy seen in {}; back-buffer draws a frame before it {:.1}, after it {:.1}; frames drawn onto with no copy seen {}",
        counts.frames,
        counts.frames_copied,
        counts.draws_before as f64 / frames as f64,
        counts.draws_after as f64 / frames as f64,
        counts.frames_uncopied_drawn
    );
}
