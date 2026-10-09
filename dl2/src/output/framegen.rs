//! FSR frame generation into VR (`mode=framegen`; same-frame stereo on D3D12).
//!
//! Each eye is its own stream of rendered frames with its own depth, motion vectors and camera (the
//! per-eye temporal history makes the game's motion vectors relative to that eye's previous frame),
//! so each eye gets its own frame-generation context ([`ffx::FrameGen`]). At an eye's present the
//! frame is copied into a ring and the frame halfway between it and that eye's previous frame is
//! generated. After the right eye, the generated pair goes out at once with the head pose halfway
//! between the two real pairs' poses, and the real pair half a frame later from a pacing thread, so
//! the headset gets twice the rendered rate, evenly spaced.
//!
//! Depth and motion vectors are copied on the game's own command list when it tags them for DLSS
//! (the render eye is known then); the camera comes with the DLSS constants.
//!
//! The HUD is kept out of the interpolation: in gameplay the game also tags each eye's UI layer, so
//! the world is recovered from the frame and that layer, interpolated, and the newest real frame's
//! layer is laid over the generated frame ([`HudComposer`]). (The game's own HUD-less frame is black
//! while its frame generation is held off, and frame generation's own HUD handling, given it,
//! returned the newest real frame unchanged: measured 2026-10-05.) Menus tag no UI layer; their
//! finished frames are interpolated.

use monaka_channel::d3d12::{self, Recorder};
use monaka_framegen::hud12::HudComposer;
use monaka_framegen::{EyeStream, FrameInputs, SLOTS, fitting_stream, skip_eye as skip};
use monaka_framegen::ffx::{Camera, Size2};
use crate::view::scene::{RENDER_EYE, TEMPORAL_EPOCH};
use crate::output::render12;
use monaka_core::protocol::HeadPose;
use monaka_producer::log;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_R8G8B8A8_TYPELESS, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R16G16_FLOAT, DXGI_FORMAT_R16G16_TYPELESS,
    DXGI_FORMAT_R32_TYPELESS,
};

pub static GENERATED: AtomicU64 = AtomicU64::new(0);
pub static REAL: AtomicU64 = AtomicU64::new(0);
static INPUT_COPIES: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static CAMERAS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static UI_TAGS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static HUD_FREE: AtomicU64 = AtomicU64::new(0);
/// Pairs whose frames got our HUD layer laid over.
static HUD_LAID: AtomicU64 = AtomicU64::new(0);

pub use monaka_framegen::{enable, enabled};

/// One eye's inputs for its next frame, written while that eye renders.
#[derive(Default)]
struct Inputs {
    depth: Option<ID3D12Resource>,
    motion: Option<ID3D12Resource>,
    motion_format: DXGI_FORMAT,
    camera: Option<Camera>,
    /// The UI layer (gameplay only: tagged for frame generation) as the game tagged it: complete
    /// only by the eye's present ("valid until present"), so it is copied there; and its copy.
    ui_tagged: Option<(ID3D12Resource, D3D12_RESOURCE_STATES)>,
    /// The depth and motion vectors of the frame-generation call, likewise copied at the present
    /// (over the copies made when DLSS tagged them).
    depth_tagged: Option<(ID3D12Resource, D3D12_RESOURCE_STATES)>,
    motion_tagged: Option<(ID3D12Resource, D3D12_RESOURCE_STATES)>,
    ui: Option<ID3D12Resource>,
    /// The inputs that arrived since the eye's last present.
    fresh_depth: bool,
    fresh_motion: bool,
    fresh_ui: bool,
}

/// What the game tagged.
#[derive(Clone, Copy, PartialEq)]
pub enum Input {
    Depth,
    MotionVectors,
    /// The HUD alone (premultiplied colour and alpha).
    Ui,
}

static INPUTS: Mutex<[Option<Inputs>; 2]> = Mutex::new([None, None]);

fn render_eye() -> Option<usize> {
    let eye = RENDER_EYE.load(Ordering::Acquire);
    (1..=2).contains(&eye).then(|| (eye - 1) as usize)
}

fn device() -> Option<ID3D12Device> {
    d3d12::device_of(&render12::game_queue()?).ok()
}

/// The texture in `slot` with `format` and `source`'s size, made (again) if it does not fit.
fn fitting(slot: &mut Option<ID3D12Resource>, source: &D3D12_RESOURCE_DESC, format: DXGI_FORMAT, flags: D3D12_RESOURCE_FLAGS) -> Option<ID3D12Resource> {
    d3d12::fitting(slot, &device()?, source.Width, source.Height, format, flags)
}

/// What the game just tagged, for the eye rendering now: copied on its command list, or (a buffer
/// of the frame-generation call, `late`) kept to be copied at the eye's present.
pub fn copy_input(list: &ID3D12GraphicsCommandList, input: Input, source: &ID3D12Resource, state: D3D12_RESOURCE_STATES, late: bool) {
    let Some(eye) = render_eye() else { return };
    let mut inputs = INPUTS.lock().unwrap_or_else(|e| e.into_inner());
    let inputs = inputs[eye].get_or_insert_with(Inputs::default);
    // SAFETY: reads a descriptor.
    let desc = unsafe { source.GetDesc() };
    let late = late && !crate::config::debug(crate::config::debug::LATE_INPUTS_AT_TAG);
    match input {
        Input::Depth if late => {
            inputs.depth_tagged = Some((source.clone(), state));
            inputs.fresh_depth = true;
        }
        Input::MotionVectors if late => {
            inputs.motion_tagged = Some((source.clone(), state));
            inputs.motion_format = desc.Format;
            inputs.fresh_motion = true;
        }
        Input::Depth => {
            let Some(target) = fitting(&mut inputs.depth, &desc, DXGI_FORMAT_R32_TYPELESS, D3D12_RESOURCE_FLAG_NONE) else { return };
            d3d12::copy_plane0(list, &target, source, state);
            inputs.fresh_depth = true;
        }
        Input::MotionVectors => {
            let Some(target) = fitting(&mut inputs.motion, &desc, desc.Format, D3D12_RESOURCE_FLAG_NONE) else { return };
            d3d12::copy_whole(list, &target, source, state);
            inputs.motion_format = desc.Format;
            inputs.fresh_motion = true;
        }
        // With the HUD kept out of the frame by `eng_chr::hudlayer`, the game's layer is empty.
        Input::Ui if eng_chr::hudlayer::redirecting() => return,
        Input::Ui => {
            inputs.ui_tagged = Some((source.clone(), state));
            inputs.fresh_ui = true;
            UI_TAGS[eye].fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    INPUT_COPIES[eye].fetch_add(1, Ordering::Relaxed);
}

/// The camera the eye rendering now was drawn with (the DLSS constants).
pub fn set_camera(camera: Camera) {
    let Some(eye) = render_eye() else { return };
    INPUTS.lock().unwrap_or_else(|e| e.into_inner())[eye].get_or_insert_with(Inputs::default).camera = Some(camera);
    CAMERAS[eye].fetch_add(1, Ordering::Relaxed);
}

/// One eye's generator and output ([`EyeStream`]), with the world it interpolates when the HUD is
/// kept out.
struct Stream {
    eye: EyeStream,
    /// The world without its HUD, interpolated in place of the frame: a ring like the frame copies,
    /// because frame generation reads the previous frame from its own texture (measured: with one
    /// texture rewritten each frame, the "generated" frame was the newest one).
    world: [ID3D12Resource; SLOTS],
    /// The ring slot written last.
    world_slot: usize,
    epoch: u32,
    /// What the history holds: frames without their HUD (true) or finished frames.
    hud_free: bool,
}

/// The HUD for this eye's present: the layer `eng_chr::hudlayer` made (kept out of the frame)
/// and how to lay it over (`render12::hud_params`).
pub struct Hud<'a> {
    pub layer: &'a ID3D12Resource,
    pub params: crate::output::warp12::Params<'a>,
}

struct State {
    recorder: Recorder,
    streams: [Option<Stream>; 2],
    real: [[Option<ID3D12Resource>; 2]; SLOTS],
    /// The real frames with the HUD layer laid over (published in place of `real` when made).
    shown: [[Option<ID3D12Resource>; 2]; SLOTS],
    /// Each eye's generated frame with the HUD layer laid over.
    generated_shown: [Option<ID3D12Resource>; 2],
    /// Whether this pair's frames got the HUD laid over, per eye.
    hud_laid: [bool; 2],
    /// Lays our HUD layer over frames (made on first use; none if the GPU cannot).
    overlay: Option<Result<crate::output::warp12::HudOverlay, ()>>,
    slot: usize,
    /// Each eye's frame this pair was generated.
    generated: [bool; 2],
    previous_pose: Option<HeadPose>,
    last_pair: Option<Instant>,
    interval: Duration,
    pacer: Option<(Sender<Pending>, std::thread::JoinHandle<()>)>,
    /// Lays the UI layer over generated frames (made on first use; none if the GPU cannot).
    composer: Option<Result<HudComposer, ()>>,
}

// SAFETY: the D3D objects are free-threaded; the contexts are used under the state's lock only.
unsafe impl Send for State {}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// A real pair waiting for its time.
struct Pending {
    due: Instant,
    eyes: [ID3D12Resource; 2],
    pose: HeadPose,
}

fn pace(pending: Receiver<Pending>) {
    while let Ok(pair) = pending.recv() {
        let now = Instant::now();
        if pair.due > now {
            std::thread::sleep(pair.due - now);
        }
        render12::publish_pair(&pair.eyes, pair.pose);
        REAL.fetch_add(1, Ordering::Relaxed);
    }
}

fn create_stream(device: &ID3D12Device, frame: &D3D12_RESOURCE_DESC, render_size: Size2) -> Result<Stream, String> {
    let eye = EyeStream::new(device, frame, render_size)?;
    let make = || d3d12::texture(device, frame.Width as u32, frame.Height, frame.Format, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS).map_err(|e| e.to_string());
    let world = [make()?, make()?, make()?];
    Ok(Stream { eye, world, world_slot: 0, epoch: 0, hud_free: false })
}

/// At eye `eye`'s (1 or 2) present: keep the frame and generate the one before it. True when the
/// generated frame is usable (the eye's previous frame was real and continuous).
fn eye_present(state: &mut State, queue: &ID3D12CommandQueue, eye: usize, buffer: &ID3D12Resource, hud: Option<&Hud>) -> bool {
    let index = eye - 1;
    state.hud_laid[index] = false;
    // SAFETY: reads a descriptor.
    let frame = unsafe { buffer.GetDesc() };
    let Some(device) = device() else { return skip("no device") };
    let Some(real) = fitting(&mut state.real[state.slot][index], &frame, frame.Format, D3D12_RESOURCE_FLAG_NONE) else { return skip("no frame copy") };
    if hud.is_some() && state.overlay.is_none() {
        state.overlay = Some(crate::output::warp12::HudOverlay::new(&device).map_err(|why| log!("HUD lay-over in frame generation unavailable ({why})")));
    }
    let overlay_ready = matches!(state.overlay, Some(Ok(_)));
    let hud = hud.filter(|_| overlay_ready);
    let (inputs, motion_format, hud_free, late_depth, late_motion) = {
        let mut inputs = INPUTS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(inputs) = inputs[index].as_mut() else { return skip("no inputs yet") };
        let fresh = inputs.fresh_depth && inputs.fresh_motion;
        // The frame-generation call's depth and motion vectors: copied now (below), on the queue
        // after the frame's own work.
        let late_depth = inputs.depth_tagged.take().and_then(|(source, state)| {
            // SAFETY: reads a descriptor.
            let d = unsafe { source.GetDesc() };
            fitting(&mut inputs.depth, &d, DXGI_FORMAT_R32_TYPELESS, D3D12_RESOURCE_FLAG_NONE).map(|target| (source, state, target))
        });
        let late_motion = inputs.motion_tagged.take().and_then(|(source, state)| {
            // SAFETY: reads a descriptor.
            let d = unsafe { source.GetDesc() };
            fitting(&mut inputs.motion, &d, d.Format, D3D12_RESOURCE_FLAG_NONE).map(|target| (source, state, target))
        });
        // This frame's UI layer: copied now, on the queue after the frame's own work.
        let hud_free = match inputs.ui_tagged.take() {
            Some(tagged) if inputs.fresh_ui => {
                // SAFETY: reads a descriptor.
                let d = unsafe { tagged.0.GetDesc() };
                // Typed like the back buffer (the game's is typeless).
                let format = if d.Format == DXGI_FORMAT_R8G8B8A8_TYPELESS { DXGI_FORMAT_R8G8B8A8_UNORM } else { d.Format };
                fitting(&mut inputs.ui, &d, format, D3D12_RESOURCE_FLAG_NONE).map(|ui| (tagged, ui))
            }
            _ => None,
        };
        (inputs.fresh_depth, inputs.fresh_motion, inputs.fresh_ui) = (false, false, false);
        let stream = state.streams[index].as_mut().map(|s| &mut s.eye);
        let Some(frame_inputs) = FrameInputs::fresh(fresh, inputs.depth.as_ref(), inputs.motion.as_ref(), inputs.camera, stream) else { return false };
        (frame_inputs, inputs.motion_format, hud_free, late_depth, late_motion)
    };
    let FrameInputs { depth, motion, camera, render_size } = inputs;
    let stream = match fitting_stream(&mut state.streams[index], &state.recorder, |s| s.eye.fits(&frame, render_size), || create_stream(&device, &frame, render_size)) {
        Ok(stream) => stream,
        Err(why) => {
            log!("frame generation unavailable: {why}");
            return false;
        }
    };
    let epoch = TEMPORAL_EPOCH.load(Ordering::Acquire);
    let same = |t: &ID3D12Resource| {
        // SAFETY: reads a descriptor.
        let d = unsafe { t.GetDesc() };
        d.Width == frame.Width && d.Height == frame.Height && d.Format == frame.Format
    };
    if hud_free.is_some() && state.composer.is_none() {
        state.composer = Some(HudComposer::new(&device).map_err(|why| log!("HUD kept out of frame generation: unavailable ({why})")));
    }
    let composer_ready = matches!(state.composer, Some(Ok(_)));
    let hud_free = hud_free.filter(|(_, ui)| composer_ready && same(ui) && frame.Format == DXGI_FORMAT_R8G8B8A8_UNORM && !crate::config::debug(crate::config::debug::NO_HUDLESS));
    let reset = stream.epoch != epoch || stream.hud_free != hud_free.is_some();
    (stream.epoch, stream.hud_free) = (epoch, hud_free.is_some());
    let Ok(Some(list)) = state.recorder.begin() else { return skip("command lists busy") };
    stream.world_slot = state.slot;
    let world = stream.world[state.slot].clone();
    d3d12::copy_whole(&list, &real, buffer, D3D12_RESOURCE_STATE_PRESENT);
    // Our HUD layer over the real frame (the frame has no HUD of its own): what is shown.
    let uav = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
    if let (Some(hud), Some(shown), Some(Ok(overlay))) = (hud, fitting(&mut state.shown[state.slot][index], &frame, DXGI_FORMAT_R8G8B8A8_UNORM, uav), state.overlay.as_mut()) {
        overlay.lay(&list, &real, hud.layer, &shown, &hud.params, index);
        state.hud_laid[index] = true;
    }
    if let Some((source, state, target)) = &late_depth {
        d3d12::copy_plane0(&list, target, source, *state);
    }
    if let Some((source, state, target)) = &late_motion {
        d3d12::copy_whole(&list, target, source, *state);
    }
    if let (Some(((ui_source, ui_state), ui)), Some(Ok(composer))) = (&hud_free, state.composer.as_mut()) {
        d3d12::copy_whole(&list, ui, ui_source, *ui_state);
        composer.unblend(&list, index, ui, &real, &world);
    }
    let motion_view = if motion_format == DXGI_FORMAT_R16G16_TYPELESS { Some(DXGI_FORMAT_R16G16_FLOAT) } else { None };
    if hud_free.is_some() {
        HUD_FREE.fetch_add(1, Ordering::Relaxed);
    }
    // What is interpolated: the world without its HUD when the game gave its UI layer, else the frame.
    let source = if hud_free.is_some() { &world } else { &real };
    let generated = stream.eye.generate(&list, &camera, &depth, &motion, motion_view, source, reset, 16.7);
    if generated.is_ok()
        && !crate::config::debug(crate::config::debug::NO_HUD_COMPOSE)
        && let (Some((_, ui)), Some(Ok(composer))) = (&hud_free, state.composer.as_mut())
    {
        composer.compose(&list, index, ui, &stream.eye.output);
    }
    // Our HUD layer over the generated frame too.
    if generated.is_ok()
        && state.hud_laid[index]
        && let (Some(hud), Some(shown), Some(Ok(overlay))) = (hud, fitting(&mut state.generated_shown[index], &frame, DXGI_FORMAT_R8G8B8A8_UNORM, uav), state.overlay.as_mut())
    {
        overlay.lay(&list, &stream.eye.output, hud.layer, &shown, &hud.params, index);
    }
    if let Err(e) = state.recorder.submit(queue) {
        return skip(&format!("submit: {e}"));
    }
    match generated {
        Ok(true) => true,
        Ok(false) => skip("history reset"),
        Err(why) => skip(&why),
    }
}
/// What the present path should do after an eye was handled.
pub enum Outcome {
    /// Keep going: the pair is not complete or was published here.
    Handled,
    /// Frame generation could not run for this eye; publish the old way.
    Unavailable,
}

/// The left eye's present.
pub fn left(queue: &ID3D12CommandQueue, buffer: &ID3D12Resource, hud: Option<&Hud>) -> Outcome {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        match device().map(|d| Recorder::new(&d, 6)) {
            Some(Ok(recorder)) => {
                *guard = Some(State {
                    recorder,
                    streams: [None, None],
                    real: Default::default(),
                    shown: Default::default(),
                    generated_shown: [None, None],
                    hud_laid: [false; 2],
                    overlay: None,
                    slot: 0,
                    generated: [false; 2],
                    previous_pose: None,
                    last_pair: None,
                    interval: Duration::from_millis(28),
                    pacer: None,
                    composer: None,
                });
            }
            _ => return Outcome::Unavailable,
        }
    }
    let state = guard.as_mut().expect("made above");
    state.generated[0] = eye_present(state, queue, 1, buffer, hud);
    Outcome::Handled
}

/// The right eye's present: the generated pair now, the real pair half a frame later.
pub fn right(queue: &ID3D12CommandQueue, buffer: &ID3D12Resource, pose: HeadPose, hud: Option<&Hud>) -> Outcome {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = guard.as_mut() else { return Outcome::Unavailable };
    state.generated[1] = eye_present(state, queue, 2, buffer, hud);
    let slot = state.slot;
    // The frames with the HUD laid over when both eyes got it, else the frames as presented.
    let hud_laid = state.hud_laid == [true, true];
    let pair = |frames: &[Option<ID3D12Resource>; 2]| frames[0].clone().zip(frames[1].clone());
    let Some((left, right)) = (if hud_laid { pair(&state.shown[slot]) } else { None }).or_else(|| pair(&state.real[slot])) else { return Outcome::Unavailable };
    if hud_laid && HUD_LAID.fetch_add(1, Ordering::Relaxed) == 0 {
        log!("frame generation: the HUD layer (the game's HUD draws, kept out of the frame) is laid over the real and the generated frames; the pieces go to the hands");
    }
    state.slot = (slot + 1) % SLOTS;
    let now = Instant::now();
    if let Some(last) = state.last_pair {
        let gap = (now - last).min(Duration::from_millis(100));
        state.interval = state.interval.mul_f32(0.8) + gap.mul_f32(0.2);
    }
    state.last_pair = Some(now);
    let previous = state.previous_pose.replace(pose);
    let generated = state.generated == [true, true];
    if generated && let (Some(previous), Some(l), Some(r)) = (previous, state.streams[0].as_ref(), state.streams[1].as_ref()) {
        let mut halfway = monaka_framegen::blend(&previous, &pose, 0.5);
        // Marks a generated pair for PC checks (the probe); gaze is otherwise unused.
        halfway.gaze.sample_time = -1;
        let shown = if crate::config::debug(crate::config::debug::SHOW_HUD_FREE) {
            // Debug: each eye's world without the HUD in place of the generated frame.
            [l.world[l.world_slot].clone(), r.world[r.world_slot].clone()]
        } else if hud_laid && let Some((gl, gr)) = pair(&state.generated_shown) {
            [gl, gr]
        } else {
            [l.eye.output.clone(), r.eye.output.clone()]
        };
        render12::publish_pair(&shown, halfway);
        GENERATED.fetch_add(1, Ordering::Relaxed);
        if state.pacer.is_none() {
            let (sender, receiver) = channel();
            let thread = std::thread::Builder::new().name("Monaka VR frame pacing".into()).spawn(move || pace(receiver));
            match thread {
                Ok(thread) => state.pacer = Some((sender, thread)),
                Err(e) => log!("frame pacing thread: {e}"),
            }
        }
        if let Some((pacer, _)) = &state.pacer {
            let due = now + (state.interval / 2).clamp(Duration::from_millis(2), Duration::from_millis(40));
            if pacer.send(Pending { due, eyes: [left.clone(), right.clone()], pose }).is_ok() {
                return Outcome::Handled;
            }
        }
    }
    render12::publish_pair(&[left, right], pose);
    REAL.fetch_add(1, Ordering::Relaxed);
    Outcome::Handled
}

/// Stops the pacing thread and frees everything once the GPU is done with it.
pub fn close() {
    let Some(mut state) = STATE.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    if let Some((sender, thread)) = state.pacer.take() {
        drop(sender);
        let _ = thread.join();
    }
    monaka_framegen::free_when_idle(state, |s| &s.recorder);
}

pub fn report() -> String {
    format!(
        "frame generation: generated pairs={}, real pairs={}, eyes not generated={}, inputs copied L/R={}/{}, cameras L/R={}/{}, UI layers L/R={}/{}, frames with the HUD kept out={}",
        GENERATED.load(Ordering::Relaxed),
        REAL.load(Ordering::Relaxed),
        monaka_framegen::eyes_skipped(),
        INPUT_COPIES[0].load(Ordering::Relaxed),
        INPUT_COPIES[1].load(Ordering::Relaxed),
        CAMERAS[0].load(Ordering::Relaxed),
        CAMERAS[1].load(Ordering::Relaxed),
        UI_TAGS[0].load(Ordering::Relaxed),
        UI_TAGS[1].load(Ordering::Relaxed),
        HUD_FREE.load(Ordering::Relaxed)
    )
}
