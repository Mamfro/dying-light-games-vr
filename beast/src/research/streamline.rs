//! `probe_streamline=1` (D3D12): the DLSS inputs the renderer hands Streamline (`crate::output::streamline`),
//! the first frames' tags, camera constants and evaluates logged with the eye being rendered
//! (changing nothing), and a check of the game's `clipToPrevClip` against the cameras. The
//! Streamline hooks also feed `probe_motion` ([`super::motion`]).

use super::options;
use monaka_hook::mem;
use monaka_producer::log;
use monaka_streamline::{Constants, Tag, constants as at, viewport_id};
use std::ffi::c_void;
use windows::core::Interface;

const LOGGED_CALLS: u64 = 12;

/// The game's tag call `n` (`slSetTagForFrame`), its tags read.
pub fn tagged(n: u64, frame: usize, viewport: *const u8, tags: &[Tag], commands: *mut c_void) {
    let o = options();
    if o.motion && !commands.is_null() {
        let raw = commands;
        // SAFETY: the game's command list, being recorded for this frame.
        if let Some(list) = unsafe { windows::Win32::Graphics::Direct3D12::ID3D12GraphicsCommandList::from_raw_borrowed(&raw) } {
            for tag in tags {
                if let Some(texture) = tag.texture() {
                    super::motion::at_tag(list, tag.kind, texture, tag.state());
                }
            }
        }
    }
    if o.streamline && n <= LOGGED_CALLS {
        let label = format!("sl tag call {n} (frame {frame:#x}, viewport {:?}, eye {:?}, commands {})", viewport_id(viewport), crate::view::stereo::rendering_eye(), !commands.is_null());
        monaka_streamline::log_tags(&label, tags);
    }
}

/// The game's camera constants (`slSetConstants`), as it sends them.
pub fn constants(constants: *const u8) {
    if options().motion && !constants.is_null() {
        super::motion::at_constants(constants as usize);
    }
}

/// The game's constants call `n` passed on unchanged (neither per-eye DLSS nor `dlss_reset` took
/// it).
pub fn unchanged(n: u64, constants: *const u8, frame: usize, viewport: *const u8) {
    if options().streamline
        && n <= 2 * LOGGED_CALLS
        && let Some(c) = Constants::read(constants)
    {
        check_clip_to_prev(&c, n);
        if n <= LOGGED_CALLS {
            log!(
                "sl constants {n} (frame {frame:#x}, viewport {:?}, eye {:?}): near {} far {} fov {} aspect {} jitter {:?} mvec scale {:?} pos {:?} fwd {:?}; \
                 depthInverted {} cameraMotionIncluded {} motionVectors3D {} reset {}",
                viewport_id(viewport),
                crate::view::stereo::rendering_eye(),
                c.f32(at::NEAR),
                c.f32(at::FAR),
                c.f32(at::FOV),
                c.f32(at::ASPECT),
                c.vec2(at::JITTER),
                c.vec2(at::MVEC_SCALE),
                c.vec3(at::POSITION),
                c.vec3(at::FORWARD),
                c.byte(at::DEPTH_INVERTED),
                c.byte(at::CAMERA_MOTION_INCLUDED),
                c.byte(at::MOTION_VECTORS_3D),
                c.byte(at::RESET),
            );
            log!("    viewToClip {:?}", c.matrix(at::CAMERA_VIEW_TO_CLIP));
            log!("    clipToPrevClip {:?}", c.matrix(at::CLIP_TO_PREV_CLIP));
        }
    }
}

/// `slEvaluateFeature` call `n`, observed: the feature, its inputs (each a structure with a 32-byte
/// header: next, GUID, version) and the command list.
pub fn evaluated(n: u64, feature: u32, frame: *const u8, inputs: *const *const u8, count: u32, commands: *mut c_void) {
    if options().streamline && n <= LOGGED_CALLS {
        let frame_index = mem::read::<u32>(frame as usize).unwrap_or(u32::MAX);
        let mut described = Vec::new();
        for i in 0..count.min(8) as usize {
            let Some(input) = mem::read::<usize>(inputs as usize + i * 8).filter(|&p| p != 0) else { continue };
            let guid = mem::read::<[u8; 16]>(input + 8).unwrap_or_default();
            let version = mem::read::<u64>(input + 24).unwrap_or(u64::MAX);
            let first = mem::read::<u32>(input + 32).unwrap_or(u32::MAX);
            described.push(format!("{{guid {guid:02x?} v{version} first u32 {first}}}"));
        }
        log!("sl evaluate {n}: feature {feature}, frame {frame_index}, {count} inputs {}, commands {commands:?}, eye {:?}", described.join(" "), crate::view::stereo::rendering_eye());
    }
}

static LAST_VIEW_PROJECTION: std::sync::Mutex<Option<[f32; 16]>> = std::sync::Mutex::new(None);

/// Whether `clipToPrevClip` as computed here from this frame's and the last frame's cameras
/// matches the game's (as given, or transposed): the check before giving each eye its own previous.
fn check_clip_to_prev(constants: &Constants, n: u64) {
    let Some(now) = constants.view_projection() else { return };
    let previous = LAST_VIEW_PROJECTION.lock().unwrap_or_else(|e| e.into_inner()).replace(now);
    let (Some(previous), game) = (previous, constants.matrix(at::CLIP_TO_PREV_CLIP)) else { return };
    let Some(inverse) = monaka_core::depth::invert(&now) else { return };
    let mine = monaka_core::depth::multiply(&inverse, &previous);
    let transposed: [f32; 16] = std::array::from_fn(|i| mine[(i % 4) * 4 + i / 4]);
    let error = |m: &[f32; 16]| m.iter().zip(&game).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    log!("clipToPrevClip check {n}: max error {:.5} as computed, {:.5} transposed (game's [8] = {:.4}, mine {:.4} / {:.4})", error(&mine), error(&transposed), game[8], mine[8], transposed[8]);
}
