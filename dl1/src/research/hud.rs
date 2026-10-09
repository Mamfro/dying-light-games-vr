//! The HUD's probes, at the HUD draws (`crate::hud::panels`): `probe_hud` logs one frame's HUD draws in
//! order (vertices, viewport, scissor, blending, pixel shader, the textures read), how one draw
//! gets its geometry, and dumps the panels; `hud_hide=a-b` leaves the frame's HUD draws `a` to
//! `b` undrawn.

use super::options;
use monaka_channel::d3d;
use monaka_producer::log;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::Interface;

/// The frames whose HUD draws `probe_hud` logs (the first also dumps the panels), and the frame
/// whose draw's geometry it reads.
const PROBED_FRAMES: [u64; 2] = [600, 3000];
const PROBE_VERTICES_FRAME: u64 = 900;

/// HUD draw `index` of frame `frame` on the immediate context: logged when probing; true when it
/// is to stay undrawn (`hud_hide`).
pub fn draw(context: &ID3D11DeviceContext, frame: u64, index: u32, vertices: u32) -> bool {
    let o = options();
    if o.hud && PROBED_FRAMES.contains(&frame) {
        log_draw(context, frame, index, vertices);
    }
    o.hud_hide.is_some_and(|(first, last)| (first..=last).contains(&index))
}

/// A HUD draw about to run into a panel: draw `index` of frame `frame`.
pub fn panel_draw(context: &ID3D11DeviceContext, device: &ID3D11Device, frame: u64, index: u32) {
    if options().hud && frame == PROBE_VERTICES_FRAME {
        probe_vertices(context, device, index);
    }
}

/// Piece `name`'s resolved panel `out` at the end of frame `frame`.
pub fn panel(context: &ID3D11DeviceContext, device: &ID3D11Device, out: &ID3D11Texture2D, frame: u64, name: &str) {
    if options().hud && frame == PROBED_FRAMES[0] {
        dump_panel(context, device, out, &super::dir().join(format!("hud-panel-{name}.bmp")));
    }
}

/// A resolved panel as a BMP, alpha shown over grey.
fn dump_panel(context: &ID3D11DeviceContext, device: &ID3D11Device, out: &ID3D11Texture2D, path: &std::path::Path) {
    match d3d::read_image(device, context, out) {
        Ok((width, height, rgba)) => {
            let bgra: Vec<u8> = rgba
                .chunks_exact(4)
                .flat_map(|p| {
                    let back = (128 * (255 - p[3] as u32) / 255) as u8;
                    [p[2].saturating_add(back), p[1].saturating_add(back), p[0].saturating_add(back), 255]
                })
                .collect();
            match std::fs::write(path, monaka_core::bmp::bgra(width, height, &bgra)) {
                Ok(()) => log!("HUD panel dumped to {}", path.display()),
                Err(e) => log!("HUD panel dump failed: {e}"),
            }
        }
        Err(e) => log!("HUD panel read-back failed: {e}"),
    }
}

/// How a HUD draw gets its geometry (finding where it lands before it is drawn): the call's
/// arguments, the input assembler's buffers, the first vertices it uses (as floats) and the start
/// of the vertex shader's first constant buffer. Reads the buffers back at once.
fn probe_vertices(ctx: &ID3D11DeviceContext, device: &ID3D11Device, index: u32) {
    let args = crate::hud::draws::draw_args();
    let mut vbs = [None];
    let (mut strides, mut offsets) = ([0u32], [0u32]);
    let mut ib = None;
    let mut ib_format = DXGI_FORMAT::default();
    let mut ib_offset = 0u32;
    let mut cbs: [Option<ID3D11Buffer>; 2] = [None, None];
    // SAFETY: reads the game's bound state into locals on its own context and thread.
    let topology = unsafe {
        ctx.IAGetVertexBuffers(0, 1, Some(vbs.as_mut_ptr()), Some(strides.as_mut_ptr()), Some(offsets.as_mut_ptr()));
        ctx.IAGetIndexBuffer(Some(&mut ib), Some(&mut ib_format), Some(&mut ib_offset));
        ctx.VSGetConstantBuffers(0, Some(&mut cbs));
        ctx.IAGetPrimitiveTopology()
    };
    let describe = |b: &ID3D11Buffer| {
        let mut desc = D3D11_BUFFER_DESC::default();
        // SAFETY: reads the buffer's description into a local.
        unsafe { b.GetDesc(&mut desc) };
        format!("{:#x} {} bytes usage {} cpu {:#x}", b.as_raw() as usize, desc.ByteWidth, desc.Usage.0, desc.CPUAccessFlags)
    };
    let (stride, vb_offset) = (strides[0], offsets[0]);
    // The first indices (or vertex numbers) the draw uses.
    let index_size = if ib_format == DXGI_FORMAT_R32_UINT { 4 } else { 2 };
    let used: Vec<u32> = match (&ib, args.indexed) {
        (Some(ib), true) => read_buffer(device, ctx, ib, ib_offset + args.start * index_size, 6.min(args.count) * index_size)
            .map(|bytes| bytes.chunks_exact(index_size as usize).map(|c| if index_size == 4 { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) } else { u16::from_le_bytes([c[0], c[1]]) as u32 }).collect())
            .unwrap_or_default(),
        _ => (args.start..args.start + 6.min(args.count)).collect(),
    };
    let vertices: Vec<String> = match &vbs[0] {
        Some(vb) if stride > 0 => used
            .iter()
            .filter_map(|&i| {
                let at = vb_offset as i64 + (i as i64 + args.base as i64) * stride as i64;
                let bytes = read_buffer(device, ctx, vb, u32::try_from(at).ok()?, stride.min(64))?;
                let floats: Vec<String> = bytes.chunks_exact(4).map(|c| format!("{:.3}", f32::from_le_bytes([c[0], c[1], c[2], c[3]]))).collect();
                Some(format!("v{i} [{}]", floats.join(" ")))
            })
            .collect(),
        _ => Vec::new(),
    };
    let constants = cbs[0]
        .as_ref()
        .and_then(|cb| read_buffer(device, ctx, cb, 0, 64))
        .map(|bytes| bytes.chunks_exact(4).map(|c| format!("{:.3}", f32::from_le_bytes([c[0], c[1], c[2], c[3]]))).collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    log!(
        "hud vertices draw {index}: {args:?} topology {} vb {} stride {stride} offset {vb_offset}; ib {} fmt {} offset {ib_offset}; vs cb0 {}: {} | {}",
        topology.0,
        vbs[0].as_ref().map_or("none".into(), describe),
        ib.as_ref().map_or("none".into(), describe),
        ib_format.0,
        cbs[0].as_ref().map_or("none".into(), describe),
        vertices.join(" "),
        constants
    );
}

/// `len` bytes of `buffer` from byte `from` (within it), read back at once through a staging copy.
fn read_buffer(device: &ID3D11Device, ctx: &ID3D11DeviceContext, buffer: &ID3D11Buffer, from: u32, len: u32) -> Option<Vec<u8>> {
    let mut desc = D3D11_BUFFER_DESC::default();
    // SAFETY: reads the buffer's description into a local.
    unsafe { buffer.GetDesc(&mut desc) };
    let len = len.min(desc.ByteWidth.checked_sub(from)?);
    if len == 0 {
        return None;
    }
    let staging_desc = D3D11_BUFFER_DESC { ByteWidth: len, Usage: D3D11_USAGE_STAGING, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, ..Default::default() };
    let mut staging = None;
    // SAFETY: a staging buffer of our own; a copy of a region inside the game's buffer; mapped,
    // read within its size and unmapped.
    unsafe {
        device.CreateBuffer(&staging_desc, None, Some(&mut staging)).ok()?;
        let staging = staging?;
        let region = D3D11_BOX { left: from, right: from + len, top: 0, bottom: 1, front: 0, back: 1 };
        ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, buffer, 0, Some(&region));
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).ok()?;
        let bytes = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), len as usize).to_vec();
        ctx.Unmap(&staging, 0);
        Some(bytes)
    }
}

fn log_draw(context: &ID3D11DeviceContext, frame: u64, index: u32, vertices: u32) {
    let mut viewport = [D3D11_VIEWPORT::default()];
    let mut viewports = 1u32;
    let mut scissor = [RECT::default()];
    let mut scissors = 1u32;
    let mut blend: Option<ID3D11BlendState> = None;
    let mut blend_desc = D3D11_BLEND_DESC::default();
    let mut shader: Option<ID3D11PixelShader> = None;
    let mut views: [Option<ID3D11ShaderResourceView>; 4] = Default::default();
    // SAFETY: reads the game's bound state into locals on its own (immediate) context and thread.
    unsafe {
        context.RSGetViewports(&mut viewports, Some(viewport.as_mut_ptr()));
        context.RSGetScissorRects(&mut scissors, Some(scissor.as_mut_ptr()));
        context.OMGetBlendState(Some(&mut blend), None, None);
        if let Some(blend) = &blend {
            blend.GetDesc(&mut blend_desc);
        }
        context.PSGetShader(&mut shader, None, None);
        context.PSGetShaderResources(0, Some(&mut views));
    }
    let textures: Vec<String> = views
        .iter()
        .enumerate()
        .filter_map(|(slot, view)| {
            let view = view.as_ref()?;
            // SAFETY: COM calls on a bound shader resource view.
            let texture = unsafe { view.GetResource() }.ok()?.cast::<ID3D11Texture2D>().ok()?;
            let desc = d3d::texture_desc(&texture);
            Some(format!("t{slot} {:#x} {}x{} fmt {}", texture.as_raw() as usize, desc.Width, desc.Height, desc.Format.0))
        })
        .collect();
    let v = viewport[0];
    let s = scissor[0];
    log!(
        "hud probe frame {frame} draw {index}: {vertices} vertices, viewport {:.0},{:.0} {:.0}x{:.0}, scissor {},{}-{},{}, blend {}, ps {:#x}, {}",
        v.TopLeftX,
        v.TopLeftY,
        v.Width,
        v.Height,
        s.left,
        s.top,
        s.right,
        s.bottom,
        blend_desc.RenderTarget[0].BlendEnable.as_bool(),
        shader.as_ref().map_or(0, |s| s.as_raw() as usize),
        if textures.is_empty() { "no textures".to_string() } else { textures.join(", ") }
    );
}
