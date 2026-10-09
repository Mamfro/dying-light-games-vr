//! Per-eye FSR frame generation for alternate-eye stereo (`mode=framegen`, [`monaka_framegen`]).
//!
//! Alternate-eye renders one eye per frame: left at frames 0, 2, 4, right at 1, 3, 5. Each eye is
//! its own stream of frames two apart, so each gets its own frame-generation context. At an eye's
//! present the frame halfway between its previous and current frames is generated: that is the
//! moment of the other eye's newest real frame, so each present publishes one pair of a single
//! moment (this eye generated, the other real), each eye with its own head pose. Each eye then
//! changes every frame instead of every other one, both eyes in step, for one frame of latency.
//!
//! Inputs come from the renderer's Streamline calls for DLSS, on the render thread: the camera with
//! the constants (which also label the frame's eye: `stereo::label_frame`), the depth and motion
//! vectors with the tags, and at DLSS's evaluate, on the game's native command list, the depth
//! copied and the motion vectors carried onto this eye's own previous frame two frames back by
//! chaining the game's own over the other eye's frame between (`motion_compose.hlsl`). The camera
//! rebase per-eye DLSS uses (`motion_fix.hlsl`) takes everything as still world: it moved the
//! weapon and arms, which move with the head, by the eyes' parallax, and they ghosted (PC capture
//! 2026-10-06). The HUD goes through the interpolation (the game tags no UI layer here).

use monaka_framegen::motion::{self, MotionFix};
use monaka_channel::d3d12::{self, Recorder};
use monaka_channel::pose::HeadSource;
use monaka_core::protocol::HeadPose;
use monaka_framegen::{EyeStream, FrameInputs, SLOTS, fitting_stream, skip_eye as skip};
use monaka_framegen::ffx::Camera;
use monaka_producer::log;
use monaka_stereo::pairs12::{PairPublisher12, Step};
use monaka_streamline::{RESOURCE_TEX2D, Tag, buffer};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::sync::Mutex;
use std::time::Instant;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R32_TYPELESS};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain3;
use windows::core::Interface;

pub use monaka_framegen::{enable, enabled};

struct Counters {
    collected: [AtomicU64; 2],
    generated: AtomicU64,
    real_pairs: AtomicU64,
    unpaired: AtomicU64,
    /// Frames no eye camera drew, sent flat to both eyes.
    flat: AtomicU64,
}
static COUNTERS: Counters = Counters {
    collected: [AtomicU64::new(0), AtomicU64::new(0)],
    generated: AtomicU64::new(0),
    real_pairs: AtomicU64::new(0),
    unpaired: AtomicU64::new(0),
    flat: AtomicU64::new(0),
};

// --- Inputs, on the render thread -----------------------------------------------------------------

/// The frame being rendered: what its Streamline calls said so far.
struct Collecting {
    eye: Option<usize>,
    camera: Option<Camera>,
    reset: bool,
    depth: Option<(usize, u32)>,
    motion: Option<(usize, u32)>,
    /// The game's motion vectors of the frame before (a copy), and its eye.
    previous_motion: Option<ID3D12Resource>,
    previous_eye: Option<usize>,
    composer: Option<MotionFix>,
    broken: bool,
}

static COLLECTING: Mutex<Collecting> =
    Mutex::new(Collecting { eye: None, camera: None, reset: false, depth: None, motion: None, previous_motion: None, previous_eye: None, composer: None, broken: false });

/// One eye's inputs for its next present (copies of our own).
#[derive(Default)]
struct EyeInputs {
    depth: Option<ID3D12Resource>,
    motion: Option<ID3D12Resource>,
    camera: Option<Camera>,
    reset: bool,
    fresh: bool,
}

static INPUTS: Mutex<[Option<EyeInputs>; 2]> = Mutex::new([None, None]);

/// `slSetConstants` for a frame labelled `eye`: its camera, and whether the game reset its history.
pub fn at_constants(constants: *const u8, eye: Option<usize>) {
    if !enabled() || constants.is_null() {
        return;
    }
    let mut c = COLLECTING.lock().unwrap_or_else(|e| e.into_inner());
    (c.eye, c.camera, c.depth, c.motion) = (None, None, None, None);
    let Some(eye) = eye.filter(|&e| e < 2) else { return };
    let Some(read) = monaka_streamline::Constants::read(constants) else { return };
    c.reset = read.reset();
    c.camera = Some(monaka_framegen::camera(&read));
    c.eye = Some(eye);
}

/// `slSetTagForFrame`: the frame's depth and motion vectors (native textures and their states).
pub fn at_tags(tags: &[Tag]) {
    if !enabled() {
        return;
    }
    let mut c = COLLECTING.lock().unwrap_or_else(|e| e.into_inner());
    if c.eye.is_none() {
        return;
    }
    for tag in tags.iter().filter(|t| t.resource_type == RESOURCE_TEX2D) {
        match tag.kind {
            buffer::DEPTH => c.depth = Some((tag.native, tag.state)),
            buffer::MOTION_VECTORS => c.motion = Some((tag.native, tag.state)),
            _ => {}
        }
    }
}

/// `slEvaluateFeature` for DLSS: the depth and motion vectors are complete. On the game's native
/// command list, before DLSS: the depth copied for this frame's eye, its motion vectors chained
/// onto this eye's previous frame, and the game's own kept for the next frame's chaining.
pub fn at_evaluate(native_list: Option<ID3D12GraphicsCommandList>) {
    if !enabled() {
        return;
    }
    let mut c = COLLECTING.lock().unwrap_or_else(|e| e.into_inner());
    let (Some(eye), Some(camera), Some((depth, depth_state)), Some((motion, motion_state))) = (c.eye, c.camera, c.depth, c.motion) else {
        // A frame without its inputs breaks the chain.
        c.previous_eye = None;
        return;
    };
    let reset = c.reset;
    c.eye = None;
    let Some(list) = native_list else {
        c.previous_eye = None;
        return;
    };
    let (raw_depth, raw_motion) = (depth as *mut c_void, motion as *mut c_void);
    // SAFETY: the game's tagged textures, live while it records this frame.
    let (Some(depth), Some(motion)) = (unsafe { ID3D12Resource::from_raw_borrowed(&raw_depth) }, unsafe { ID3D12Resource::from_raw_borrowed(&raw_motion) }) else { return };
    if c.broken {
        return;
    }
    let Ok(device) = d3d12::device_of(depth) else { return };
    if c.composer.is_none() {
        match MotionFix::composer(&device) {
            Ok(composer) => c.composer = Some(composer),
            Err(why) => {
                log!("frame generation off: per-eye motion vectors: {why}");
                c.broken = true;
                return;
            }
        }
    }
    let motion_state = D3D12_RESOURCE_STATES(motion_state as i32);
    // SAFETY: reads descriptors.
    let (depth_desc, motion_desc) = unsafe { (depth.GetDesc(), motion.GetDesc()) };
    // The frame before was the other eye's, and its vectors are kept: this eye's chain holds.
    let previous = c.previous_motion.clone().filter(|p| {
        // SAFETY: reads a descriptor.
        let d = unsafe { p.GetDesc() };
        c.previous_eye == Some(1 - eye) && d.Width == motion_desc.Width && d.Height == motion_desc.Height
    });
    let mut inputs = INPUTS.lock().unwrap_or_else(|e| e.into_inner());
    let inputs = inputs[eye].get_or_insert_with(EyeInputs::default);
    let Some(depth_copy) = d3d12::fitting(&mut inputs.depth, &device, depth_desc.Width, depth_desc.Height, DXGI_FORMAT_R32_TYPELESS, D3D12_RESOURCE_FLAG_NONE) else { return };
    let Some(motion_copy) = d3d12::fitting(&mut inputs.motion, &device, motion_desc.Width, motion_desc.Height, DXGI_FORMAT_R16G16B16A16_FLOAT, D3D12_RESOURCE_FLAG_NONE) else { return };
    d3d12::copy_plane0(&list, &depth_copy, depth, D3D12_RESOURCE_STATES(depth_state as i32));
    let chained = match (&previous, c.composer.as_mut()) {
        (Some(previous), Some(composer)) => {
            d3d12::barrier(&list, previous, D3D12_RESOURCE_STATE_COMMON, motion::READABLE);
            let recorded = composer.record(&list, previous, motion, &motion::Constants::chaining([1.0, 1.0]));
            d3d12::barrier(&list, previous, motion::READABLE, D3D12_RESOURCE_STATE_COMMON);
            match recorded.and_then(|()| composer.output(motion_desc.Width as u32, motion_desc.Height)) {
                Ok(output) => {
                    d3d12::copy_whole(&list, &motion_copy, &output, motion::READABLE);
                    true
                }
                Err(why) => {
                    log!("frame generation off: per-eye motion vectors: {why}");
                    c.broken = true;
                    false
                }
            }
        }
        _ => false,
    };
    // This frame's vectors, for the next frame's chain.
    if let Some(keep) = d3d12::fitting(&mut c.previous_motion, &device, motion_desc.Width, motion_desc.Height, motion_desc.Format, D3D12_RESOURCE_FLAG_NONE) {
        d3d12::copy_whole(&list, &keep, motion, motion_state);
        c.previous_eye = Some(eye);
    }
    (inputs.camera, inputs.reset, inputs.fresh) = (Some(camera), reset || !chained, true);
    COUNTERS.collected[eye].fetch_add(1, Relaxed);
}
// --- At each present, on the present thread ---------------------------------------------------------

struct State {
    recorder: Recorder,
    publisher: PairPublisher12,
    streams: [Option<EyeStream>; 2],
    real: [[Option<ID3D12Resource>; SLOTS]; 2],
    frames: [usize; 2],
    /// Each eye's newest real frame and its pose.
    last: [Option<(ID3D12Resource, HeadPose)>; 2],
    last_eye: Option<usize>,
    /// When a labelled frame was last presented.
    last_labelled: Option<Instant>,
}

// SAFETY: the D3D objects are free-threaded; everything is used under the state's lock only.
unsafe impl Send for State {}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static CHANNEL: Mutex<Option<monaka_channel::ChannelName>> = Mutex::new(None);

/// The channel pairs go out on (set at start).
pub fn set_channel(channel: monaka_channel::ChannelName) {
    *CHANNEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(channel);
}

/// Generates eye `eye`'s frame halfway between its previous one and `real` (its current one) on
/// `list`. True when the result is usable.
fn generate(state: &mut State, list: &ID3D12GraphicsCommandList, device: &ID3D12Device, eye: usize, real: &ID3D12Resource, frame: &D3D12_RESOURCE_DESC, sequence_broken: bool) -> bool {
    let (inputs, reset) = {
        let mut inputs = INPUTS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(inputs) = inputs[eye].as_mut() else { return skip("no inputs yet") };
        let fresh = std::mem::take(&mut inputs.fresh);
        let Some(frame_inputs) = FrameInputs::fresh(fresh, inputs.depth.as_ref(), inputs.motion.as_ref(), inputs.camera, state.streams[eye].as_mut()) else { return false };
        (frame_inputs, inputs.reset)
    };
    let FrameInputs { depth, motion, camera, render_size } = inputs;
    let stream = match fitting_stream(&mut state.streams[eye], &state.recorder, |s| s.fits(frame, render_size), || EyeStream::new(device, frame, render_size)) {
        Ok(stream) => stream,
        Err(why) => return skip(&format!("no context: {why}")),
    };
    match stream.generate(list, &camera, &depth, &motion, None, real, reset || sequence_broken, 33.3) {
        Ok(true) => true,
        Ok(false) => skip("history reset"),
        Err(why) => skip(&why),
    }
}
/// The state, made on first use (command lists on the game queue's device, the channel's publisher).
fn state_for<'a>(guard: &'a mut Option<State>, queue: &ID3D12CommandQueue) -> Option<&'a mut State> {
    if guard.is_none() {
        let channel = CHANNEL.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        let device = d3d12::device_of(queue).ok()?;
        match Recorder::new(&device, 6) {
            Ok(recorder) => {
                *guard = Some(State {
                    recorder,
                    publisher: PairPublisher12::new(channel),
                    streams: [None, None],
                    real: Default::default(),
                    frames: [0; 2],
                    last: [None, None],
                    last_eye: None,
                    last_labelled: None,
                })
            }
            Err(e) => {
                log!("frame generation: no command lists ({e})");
                return None;
            }
        }
    }
    guard.as_mut()
}

/// Before the game presents eye `eye`'s frame (rendered from `pose`): keeps it, generates this eye's
/// frame of the other eye's newest moment, and publishes that pair.
pub fn present(swap: &IDXGISwapChain3, queue: &ID3D12CommandQueue, eye: usize, pose: HeadPose, head: &HeadSource, report: impl Fn(&mut HeadPose)) -> Step {
    let eye = eye.min(1);
    // SAFETY: COM calls on the game's live swapchain, on the thread that presents it.
    let Ok(buffer) = (unsafe { swap.GetBuffer::<ID3D12Resource>(swap.GetCurrentBackBufferIndex()) }) else { return Step::Failed };
    let Ok(device) = d3d12::device_of(queue) else { return Step::Failed };
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = state_for(&mut guard, queue) else { return Step::Failed };
    if !state.publisher.ready_for(&buffer, queue) {
        return Step::Failed;
    }
    // SAFETY: reads a descriptor.
    let frame = unsafe { buffer.GetDesc() };
    let slot = state.frames[eye] % SLOTS;
    let Some(real) = d3d12::fitting(&mut state.real[eye][slot], &device, frame.Width, frame.Height, frame.Format, D3D12_RESOURCE_FLAG_NONE) else { return Step::Failed };
    let Ok(Some(list)) = state.recorder.begin() else {
        skip("command lists busy");
        return Step::Failed;
    };
    d3d12::copy_whole(&list, &real, &buffer, D3D12_RESOURCE_STATE_PRESENT);
    // The same eye twice in a row: frames went missing, this eye's history no longer runs.
    let sequence_broken = state.last_eye == Some(eye);
    let generated = generate(state, &list, &device, eye, &real, &frame, sequence_broken);
    if let Err(e) = state.recorder.submit(queue) {
        log!("frame generation: submit failed: {e}");
        return Step::Failed;
    }
    let other = 1 - eye;
    let previous = state.last[eye].as_ref().map(|(_, p)| *p);
    let step = match (&state.last[other], previous) {
        // This eye's frame of the other eye's moment beside that eye's real frame.
        (Some((other_frame, other_pose)), Some(previous)) if generated => {
            let output = state.streams[eye].as_ref().map(|s| s.output.clone()).expect("generated");
            let halfway = monaka_framegen::blend(&previous, &pose, 0.5);
            let (images, poses) = order(eye, (&output, halfway), (other_frame, *other_pose));
            COUNTERS.generated.fetch_add(1, Relaxed);
            state.publisher.publish(images, poses, head, &report)
        }
        // Not generated: the pair of the two newest real frames, a frame apart (plain alternate-eye).
        (Some((other_frame, other_pose)), _) => {
            let (images, poses) = order(eye, (&real, pose), (other_frame, *other_pose));
            COUNTERS.real_pairs.fetch_add(1, Relaxed);
            state.publisher.publish(images, poses, head, &report)
        }
        (None, _) => {
            COUNTERS.unpaired.fetch_add(1, Relaxed);
            Step::Left
        }
    };
    state.last[eye] = Some((real, pose));
    state.frames[eye] += 1;
    state.last_eye = Some(eye);
    state.last_labelled = Some(Instant::now());
    step
}

/// A frame no eye camera drew (menus, the pause screen, loading: the player camera is not updated)
/// goes to both eyes, flat, with the newest head pose; the eyes' histories start over after it.
pub fn present_unlabelled(swap: &IDXGISwapChain3, queue: &ID3D12CommandQueue, head: &HeadSource, report: impl Fn(&mut HeadPose)) -> Step {
    // SAFETY: COM calls on the game's live swapchain, on the thread that presents it.
    let Ok(buffer) = (unsafe { swap.GetBuffer::<ID3D12Resource>(swap.GetCurrentBackBufferIndex()) }) else { return Step::Failed };
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = state_for(&mut guard, queue) else { return Step::Failed };
    // A lone unlabelled frame in play is skipped (a flat frame would flash); a menu has none for long.
    if state.last_labelled.is_some_and(|t| t.elapsed().as_millis() < 200) {
        return Step::Waiting;
    }
    if !state.publisher.ready_for(&buffer, queue) {
        return Step::Failed;
    }
    (state.last, state.last_eye) = ([None, None], None);
    state.streams.iter_mut().flatten().for_each(|s| s.primed = false);
    let pose = head.current().unwrap_or_default();
    COUNTERS.flat.fetch_add(1, Relaxed);
    state.publisher.publish([(&buffer, D3D12_RESOURCE_STATE_PRESENT); 2], [pose, pose], head, &report)
}

/// (left, right) images in the common state and their poses, from this eye's and the other's.
fn order<'a>(eye: usize, this: (&'a ID3D12Resource, HeadPose), other: (&'a ID3D12Resource, HeadPose)) -> ([(&'a ID3D12Resource, D3D12_RESOURCE_STATES); 2], [HeadPose; 2]) {
    let (left, right) = if eye == 0 { (this, other) } else { (other, this) };
    ([(left.0, D3D12_RESOURCE_STATE_COMMON), (right.0, D3D12_RESOURCE_STATE_COMMON)], [left.1, right.1])
}

/// Frees everything once the GPU is done with it (after the hooks are out).
pub fn close() {
    let Some(mut state) = STATE.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    log!("frame generation: {} pairs published, {} failures", state.publisher.pairs(), state.publisher.failures());
    state.publisher.close();
    monaka_framegen::free_when_idle(state, |s| &s.recorder);
    {
        let mut c = COLLECTING.lock().unwrap_or_else(|e| e.into_inner());
        (c.composer, c.previous_motion, c.previous_eye) = (None, None, None);
    }
    *INPUTS.lock().unwrap_or_else(|e| e.into_inner()) = [None, None];
}

pub fn report() -> String {
    format!(
        "frame generation: inputs L/R {}/{}, pairs with a generated eye {}, real pairs {}, eyes not generated {}, unpaired {}, flat (menus) {}",
        COUNTERS.collected[0].load(Relaxed),
        COUNTERS.collected[1].load(Relaxed),
        COUNTERS.generated.load(Relaxed),
        COUNTERS.real_pairs.load(Relaxed),
        monaka_framegen::eyes_skipped(),
        COUNTERS.unpaired.load(Relaxed),
        COUNTERS.flat.load(Relaxed)
    )
}
