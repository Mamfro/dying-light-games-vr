//! The HUD as a layer of its own, with no help from frame generation (D3D12 Chrome Engine games:
//! Dying Light 2, The Beast). The engine's render loop is a script; without frame generation its
//! `gui` step draws the HUD straight onto the SDR colour buffer, after the post-processing that
//! made it and before one full-screen draw copies it to the back buffer (the scripts in
//! `data0.pak`, `renderloop/scripts/gui.ppfx`). The game sets no markers on its command lists
//! (no `BeginEvent`), so the pass is told by the frame's shape: the game's command
//! lists are hooked (through the game queue's `ExecuteCommandLists`, which hands the first list
//! over), each list's draws are grouped by the render target bound, and a frame's groups are
//! strung in execution order.
//!
//! The HUD's group is the frame's last group of draws that are not full-screen triangles (the HUD's
//! draws onto the SDR buffer come before the full-screen copies to the back buffer).
//! Its render target, learned from one frame, names the HUD draws of the next: every draw onto it
//! that is not a full-screen triangle. Each such draw is drawn a second time onto the layer (a
//! cleared RGBA8 render target of the frame's size, bound in the game's place for that one draw),
//! so the HUD comes out premultiplied, as a frame generation UI layer would. At the present the
//! producer takes the layer ([`take_layer`]: in COMMON, for its panels and for taking the pieces
//! out of the eye), then gives it back ([`return_layer`]: cleared, a render target again for the
//! next frame). `probe` logs a few frames' groups and publishes the layer on `<channel>-panel-layer`.

use monaka_channel::ChannelName;
use monaka_channel::d3d12::{self, Recorder, barrier};
use monaka_channel::fence::FenceProducer;
use monaka_hook::{Hooks, InFlight, Original, Placement, mem};
use monaka_producer::log;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM;
use windows::core::{BOOL, Interface};

/// `ID3D12CommandQueue` and `ID3D12GraphicsCommandList` vtable slots (`windows` 0.62's layouts).
mod slot {
    pub const QUEUE_EXECUTE_COMMAND_LISTS: usize = 10;
    pub const LIST_DRAW_INSTANCED: usize = 12;
    pub const LIST_DRAW_INDEXED_INSTANCED: usize = 13;
    pub const LIST_OM_SET_RENDER_TARGETS: usize = 46;
    pub const LIST_BEGIN_EVENT: usize = 57;
}

type ExecuteFn = unsafe extern "system" fn(queue: *mut c_void, count: u32, lists: *const *mut c_void);
type DrawInstancedFn = unsafe extern "system" fn(list: *mut c_void, vertices: u32, instances: u32, first_vertex: u32, first_instance: u32);
type DrawIndexedInstancedFn = unsafe extern "system" fn(list: *mut c_void, indices: u32, instances: u32, first_index: u32, base_vertex: i32, first_instance: u32);
type SetRenderTargetsFn = unsafe extern "system" fn(list: *mut c_void, count: u32, targets: *const D3D12_CPU_DESCRIPTOR_HANDLE, single: BOOL, depth: *const D3D12_CPU_DESCRIPTOR_HANDLE);
type EventFn = unsafe extern "system" fn(list: *mut c_void, metadata: u32, data: *const c_void, size: u32);

static EXECUTE: Original<ExecuteFn> = Original::new();
static DRAW_INSTANCED: Original<DrawInstancedFn> = Original::new();
static DRAW_INDEXED_INSTANCED: Original<DrawIndexedInstancedFn> = Original::new();
static SET_RENDER_TARGETS: Original<SetRenderTargetsFn> = Original::new();
static BEGIN_EVENT: Original<EventFn> = Original::new();

/// The command-list hooks, installed from the first list the queue executes.
static LIST_HOOKS: Mutex<Option<Hooks>> = Mutex::new(None);
static LIST_HOOKED: AtomicBool = AtomicBool::new(false);
/// Set once removed: the queue hook (still in place until the producer's hooks go) hooks no more.
static REMOVED: AtomicBool = AtomicBool::new(false);
static PROBING: AtomicBool = AtomicBool::new(false);
/// The HUD draws go onto the layer instead of the frame (else onto both): the frame comes out
/// without its HUD, for a producer that lays the layer over the eyes itself.
static REDIRECT: AtomicBool = AtomicBool::new(false);

/// Whether the HUD draws are kept out of the frame (see [`install`]).
pub fn redirecting() -> bool {
    REDIRECT.load(Relaxed) && LAYER_VIEW.load(Relaxed) != 0
}

/// Counts of the hooked calls, to see the hooks work.
static CALLS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
const C_EVENTS: usize = 0;
const C_TARGETS: usize = 1;
const C_DRAWS: usize = 2;
const C_EXECUTES: usize = 3;

/// Draws onto one render target, in a row, on one list.
#[derive(Clone, Copy, Debug, Default)]
struct Group {
    target: usize,
    draws: u32,
    /// Draws of three vertices (a full-screen triangle).
    full_screen: u32,
    /// The most vertices (or indices) one draw had.
    largest: u32,
}

/// The game's latest render target binding on a list, to put back after a draw of ours.
#[derive(Clone, Copy)]
struct Bound {
    count: u32,
    targets: [D3D12_CPU_DESCRIPTOR_HANDLE; 8],
    single: BOOL,
    depth: Option<D3D12_CPU_DESCRIPTOR_HANDLE>,
}

/// One command list's groups since it was last executed, and its binding.
#[derive(Default)]
struct ListState {
    groups: Vec<Group>,
    bound: Option<Bound>,
}

static LISTS: Mutex<Vec<(usize, ListState)>> = Mutex::new(Vec::new());
/// The frame's groups so far, in execution order (lists strung as the queue executes them).
static FRAME: Mutex<Vec<Group>> = Mutex::new(Vec::new());
static FRAMES: AtomicU64 = AtomicU64::new(0);
const GROUPS_KEPT: usize = 512;

/// The HUD's render target (the descriptor handle), once learned from a frame's shape, and the
/// candidate of the latest frame (taken once two frames agree).
static HUD_TARGET: AtomicUsize = AtomicUsize::new(0);
static CANDIDATE: AtomicUsize = AtomicUsize::new(0);
/// A group of this many draws or more, none full-screen, is a HUD candidate.
const HUD_DRAWS_AT_LEAST: u32 = 4;
/// HUD draws drawn onto the layer this frame, and in all.
static LAYER_DRAWS: AtomicU64 = AtomicU64::new(0);
static LAYER_DRAWS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// The layer: a render target of the frame's size with its view, on the game's device.
struct Layer {
    device: ID3D12Device,
    texture: ID3D12Resource,
    /// Holds the view's descriptor.
    _heap: ID3D12DescriptorHeap,
    view: D3D12_CPU_DESCRIPTOR_HANDLE,
    size: (u32, u32),
    /// Bound as a render target (else in COMMON).
    render_target: bool,
    recorder: Recorder,
    /// The probe's channel for the whole layer.
    publisher: Option<FenceProducer>,
}

static LAYER: Mutex<Option<Layer>> = Mutex::new(None);
static LAYER_FAILED: AtomicBool = AtomicBool::new(false);
/// The layer's render target view, for the draws (0: no layer yet or not a render target now).
static LAYER_VIEW: AtomicUsize = AtomicUsize::new(0);
static PROBE_CHANNEL: Mutex<Option<ChannelName>> = Mutex::new(None);

impl Layer {
    fn new(device: &ID3D12Device, size: (u32, u32)) -> Result<Self, String> {
        let texture = d3d12::texture(device, size.0, size.1, DXGI_FORMAT_R8G8B8A8_UNORM, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET).map_err(|e| format!("layer texture: {e}"))?;
        let desc = D3D12_DESCRIPTOR_HEAP_DESC { Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV, NumDescriptors: 1, ..Default::default() };
        // SAFETY: creation calls on the live device; the view is of our own texture into our own heap.
        let (heap, view) = unsafe {
            let heap: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&desc).map_err(|e| format!("layer view heap: {e}"))?;
            let view = heap.GetCPUDescriptorHandleForHeapStart();
            device.CreateRenderTargetView(&texture, None, view);
            (heap, view)
        };
        let recorder = Recorder::new(device, 4).map_err(|e| format!("layer command lists: {e}"))?;
        Ok(Self { device: device.clone(), texture, _heap: heap, view, size, render_target: false, recorder, publisher: None })
    }
}

fn with_list<R>(list: usize, f: impl FnOnce(&mut ListState) -> R) -> R {
    let mut lists = LISTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, state)) = lists.iter_mut().find(|(l, _)| *l == list) {
        return f(state);
    }
    if lists.len() >= 64 {
        lists.remove(0);
    }
    lists.push((list, ListState::default()));
    f(&mut lists.last_mut().expect("pushed").1)
}

unsafe extern "system" fn begin_event(list: *mut c_void, metadata: u32, data: *const c_void, size: u32) {
    let _flight = InFlight::enter();
    CALLS[C_EVENTS].fetch_add(1, Relaxed);
    // SAFETY: forwards the game's own call.
    unsafe { BEGIN_EVENT.get()(list, metadata, data, size) }
}

unsafe extern "system" fn set_render_targets(list: *mut c_void, count: u32, targets: *const D3D12_CPU_DESCRIPTOR_HANDLE, single: BOOL, depth: *const D3D12_CPU_DESCRIPTOR_HANDLE) {
    let _flight = InFlight::enter();
    CALLS[C_TARGETS].fetch_add(1, Relaxed);
    let first = if count > 0 && !targets.is_null() { mem::read::<usize>(targets as usize).unwrap_or(0) } else { 0 };
    // The binding, to put back after a draw onto the layer (a single handle stands for a range).
    let mut bound = Bound { count: count.min(8), targets: [D3D12_CPU_DESCRIPTOR_HANDLE::default(); 8], single, depth: None };
    let kept = if single.as_bool() { 1 } else { bound.count as usize };
    for i in 0..kept {
        bound.targets[i] = D3D12_CPU_DESCRIPTOR_HANDLE { ptr: mem::read::<usize>(targets as usize + i * 8).unwrap_or(0) };
    }
    if !depth.is_null() {
        bound.depth = mem::read::<usize>(depth as usize).map(|ptr| D3D12_CPU_DESCRIPTOR_HANDLE { ptr });
    }
    with_list(list as usize, |state| {
        if state.groups.last().is_none_or(|g| g.target != first) && state.groups.len() < GROUPS_KEPT {
            state.groups.push(Group { target: first, ..Group::default() });
        }
        state.bound = Some(bound);
    });
    // SAFETY: forwards the game's own call.
    unsafe { SET_RENDER_TARGETS.get()(list, count, targets, single, depth) }
}

/// Notes a draw; whether it is a HUD draw to draw onto the layer too, with the binding to put
/// back after.
fn note_draw(list: usize, vertices: u32) -> Option<Bound> {
    CALLS[C_DRAWS].fetch_add(1, Relaxed);
    let hud = HUD_TARGET.load(Relaxed);
    with_list(list, |state| {
        if state.groups.is_empty() {
            state.groups.push(Group::default());
        }
        let group = state.groups.last_mut().expect("one group");
        group.draws += 1;
        if vertices == 3 {
            group.full_screen += 1;
        }
        group.largest = group.largest.max(vertices);
        (hud != 0 && group.target == hud && vertices != 3 && LAYER_VIEW.load(Relaxed) != 0).then_some(state.bound).flatten()
    })
}

/// Draws onto the layer with `draw`, the game's binding put back after.
///
/// # Safety
/// `list` is the game's live list being recorded, on its own thread.
unsafe fn onto_layer(list: *mut c_void, bound: &Bound, draw: impl Fn()) -> bool {
    let view = D3D12_CPU_DESCRIPTOR_HANDLE { ptr: LAYER_VIEW.load(Relaxed) };
    if view.ptr == 0 {
        return false;
    }
    let depth = bound.depth.as_ref().map_or(std::ptr::null(), |d| d as *const _);
    // SAFETY: the game's own binding call on its list with our view (then its own again); the
    // draw is the game's, repeated with the state still set.
    unsafe {
        SET_RENDER_TARGETS.get()(list, 1, &view, BOOL(0), depth);
        draw();
        SET_RENDER_TARGETS.get()(list, bound.count, bound.targets.as_ptr(), bound.single, depth);
    }
    LAYER_DRAWS.fetch_add(1, Relaxed);
    true
}

/// A draw: the game's own onto its target, and a HUD draw onto the layer too, or onto the layer
/// only when the HUD is redirected (then the game's own is skipped).
///
/// # Safety
/// `list` is the game's live list being recorded, on its own thread; `draw` is the game's call.
unsafe fn draw(list: *mut c_void, hud: Option<Bound>, draw: impl Fn()) {
    // SAFETY: the caller's.
    unsafe {
        match hud {
            Some(bound) if REDIRECT.load(Relaxed) => {
                if !onto_layer(list, &bound, &draw) {
                    draw();
                }
            }
            Some(bound) => {
                draw();
                onto_layer(list, &bound, &draw);
            }
            None => draw(),
        }
    }
}

unsafe extern "system" fn draw_instanced(list: *mut c_void, vertices: u32, instances: u32, first_vertex: u32, first_instance: u32) {
    let _flight = InFlight::enter();
    let hud = note_draw(list as usize, vertices);
    // SAFETY: the game's own call, on its list.
    unsafe { draw(list, hud, || DRAW_INSTANCED.get()(list, vertices, instances, first_vertex, first_instance)) }
}

unsafe extern "system" fn draw_indexed_instanced(list: *mut c_void, indices: u32, instances: u32, first_index: u32, base_vertex: i32, first_instance: u32) {
    let _flight = InFlight::enter();
    let hud = note_draw(list as usize, indices);
    // SAFETY: as above.
    unsafe { draw(list, hud, || DRAW_INDEXED_INSTANCED.get()(list, indices, instances, first_index, base_vertex, first_instance)) }
}

/// Hooks the first list the game queue executes (every list of its class shares the vtable), and
/// strings each executed list's groups onto the frame's.
unsafe extern "system" fn execute_command_lists(queue: *mut c_void, count: u32, lists: *const *mut c_void) {
    let _flight = InFlight::enter();
    CALLS[C_EXECUTES].fetch_add(1, Relaxed);
    if !REMOVED.load(Relaxed) && !LIST_HOOKED.load(Relaxed) && count > 0 && !lists.is_null() {
        let list = mem::read::<usize>(lists as usize).unwrap_or(0);
        if list != 0 && !LIST_HOOKED.swap(true, Relaxed) {
            // SAFETY: a live command list the game is executing, whose methods have these types.
            match unsafe { hook_list(list as *mut c_void) } {
                Ok(()) => log!("hud layer: the game's command lists are hooked (from list {list:#x})"),
                Err(why) => log!("hud layer off: command list hooks failed: {why}"),
            }
        }
    }
    if LIST_HOOKED.load(Relaxed) && !lists.is_null() {
        let mut frame = FRAME.lock().unwrap_or_else(|e| e.into_inner());
        for i in 0..count as usize {
            let Some(list) = mem::read::<usize>(lists as usize + i * 8) else { continue };
            let groups = with_list(list, |state| std::mem::take(&mut state.groups));
            for group in groups {
                if frame.len() < GROUPS_KEPT {
                    frame.push(group);
                }
            }
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { EXECUTE.get()(queue, count, lists) }
}

/// The frame's end (the producer's present hook): the HUD's target learned from the frame's
/// shape (the last group of draws that are not full-screen), and the frame's groups logged on the
/// probe's frames.
pub fn frame_end() {
    let n = FRAMES.fetch_add(1, Relaxed) + 1;
    let groups = std::mem::take(&mut *FRAME.lock().unwrap_or_else(|e| e.into_inner()));
    let candidate = groups.iter().rev().find(|g| g.draws >= HUD_DRAWS_AT_LEAST && g.full_screen == 0 && g.target != 0).map_or(0, |g| g.target);
    let before = CANDIDATE.swap(candidate, Relaxed);
    if candidate != 0 && candidate == before && HUD_TARGET.swap(candidate, Relaxed) != candidate {
        log!("hud layer: the HUD draws onto target {candidate:#x} (frame {n})");
    }
    let drawn = LAYER_DRAWS.swap(0, Relaxed);
    LAYER_DRAWS_TOTAL.fetch_add(drawn, Relaxed);
    if PROBING.load(Relaxed) && (n == 60 || n == 300) {
        log!(
            "hud layer: frame {n}: {} groups (calls so far: events {}, targets {}, draws {}, executes {})",
            groups.len(),
            CALLS[C_EVENTS].load(Relaxed),
            CALLS[C_TARGETS].load(Relaxed),
            CALLS[C_DRAWS].load(Relaxed),
            CALLS[C_EXECUTES].load(Relaxed)
        );
        for (i, g) in groups.iter().enumerate() {
            if g.draws > 0 {
                log!("  group {i}: target {:#x} draws {} full-screen {} largest {}", g.target, g.draws, g.full_screen, g.largest);
            }
        }
    }
}

/// # Safety
/// `list` must be a live `ID3D12GraphicsCommandList`.
unsafe fn hook_list(list: *mut c_void) -> Result<(), String> {
    let mut hooks = Hooks::default();
    let place = |placement: monaka_hook::Result<Placement>, name: &str| -> Result<(), String> {
        match placement {
            Ok(Placement::Vtable(why)) => {
                log!("hud layer: {name}: {why}; hooking its vtable slot instead");
                Ok(())
            }
            Ok(Placement::Code) => Ok(()),
            Err(e) => Err(format!("{name}: {e}")),
        }
    };
    // SAFETY: the caller's guarantee; each detour has its method's type.
    unsafe {
        place(hooks.method(&BEGIN_EVENT, "BeginEvent", list, slot::LIST_BEGIN_EVENT, begin_event as EventFn), "BeginEvent")?;
        place(hooks.method(&SET_RENDER_TARGETS, "OMSetRenderTargets", list, slot::LIST_OM_SET_RENDER_TARGETS, set_render_targets as SetRenderTargetsFn), "OMSetRenderTargets")?;
        place(hooks.method(&DRAW_INSTANCED, "DrawInstanced", list, slot::LIST_DRAW_INSTANCED, draw_instanced as DrawInstancedFn), "DrawInstanced")?;
        place(hooks.method(&DRAW_INDEXED_INSTANCED, "DrawIndexedInstanced", list, slot::LIST_DRAW_INDEXED_INSTANCED, draw_indexed_instanced as DrawIndexedInstancedFn), "DrawIndexedInstanced")?;
    }
    hooks.enable().map_err(|e| e.to_string())?;
    *LIST_HOOKS.lock().unwrap_or_else(|e| e.into_inner()) = Some(hooks);
    Ok(())
}

/// At the present, before the eyes are taken: the layer with this frame's HUD, in COMMON, for the
/// producer to read (its panels, the pieces out of the eye) until [`return_layer`]. `size` is the
/// frame's (the layer is made or remade to it). `None` until the HUD's target is known and the
/// layer has been drawn into.
pub fn take_layer(queue: &ID3D12CommandQueue, size: (u32, u32)) -> Option<ID3D12Resource> {
    if LAYER_FAILED.load(Relaxed) || REMOVED.load(Relaxed) || !LIST_HOOKED.load(Relaxed) || size.0 == 0 || size.1 == 0 {
        return None;
    }
    let mut slot = LAYER.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_none_or(|l| l.size != size) {
        LAYER_VIEW.store(0, Relaxed);
        if let Some(old) = slot.take() {
            old.recorder.idle(2000);
        }
        match d3d12::device_of(queue).map_err(|e| e.to_string()).and_then(|device| Layer::new(&device, size)) {
            Ok(layer) => {
                log!("hud layer: a {}x{} layer for the HUD", size.0, size.1);
                *slot = Some(layer);
            }
            Err(why) => {
                log!("hud layer off: {why}");
                LAYER_FAILED.store(true, Relaxed);
                return None;
            }
        }
    }
    let layer = slot.as_mut()?;
    if !layer.render_target {
        // Nothing drawn into it yet: it becomes a render target at the return.
        return None;
    }
    let list = layer.recorder.begin().ok()??;
    barrier(&list, &layer.texture, D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_COMMON);
    if layer.recorder.submit(queue).is_err() {
        return None;
    }
    // The view stays bound for draws recorded meanwhile: they execute after the return below on
    // the same queue.
    layer.render_target = false;
    if PROBING.load(Relaxed) {
        publish_probe(layer, queue);
    }
    Some(layer.texture.clone())
}

/// After the producer is done with the layer: cleared and a render target again, for the next
/// frame's HUD draws.
pub fn return_layer(queue: &ID3D12CommandQueue) {
    let mut slot = LAYER.lock().unwrap_or_else(|e| e.into_inner());
    let Some(layer) = slot.as_mut() else { return };
    if layer.render_target {
        return;
    }
    let Ok(Some(list)) = layer.recorder.begin() else { return };
    barrier(&list, &layer.texture, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_RENDER_TARGET);
    // SAFETY: clears our own render target through its own view, on our list.
    unsafe { list.ClearRenderTargetView(layer.view, &[0.0, 0.0, 0.0, 0.0], None) };
    if layer.recorder.submit(queue).is_ok() {
        layer.render_target = true;
        LAYER_VIEW.store(layer.view.ptr, Relaxed);
    }
}

/// The probe: the whole layer on `<channel>-panel-layer` (the harness saves it with a frame).
fn publish_probe(layer: &mut Layer, queue: &ID3D12CommandQueue) {
    if layer.publisher.is_none() {
        let name = PROBE_CHANNEL.lock().ok().and_then(|c| c.clone()).and_then(|c| c.panel("layer"));
        let Some(name) = name else { return };
        match FenceProducer::create(&name, &layer.device, queue, layer.size.0, layer.size.1, DXGI_FORMAT_R8G8B8A8_UNORM) {
            Ok(producer) => layer.publisher = Some(producer),
            Err(e) => {
                log!("hud layer probe: no channel for the layer: {e}");
                PROBING.store(false, Relaxed);
                return;
            }
        }
    }
    if let Some(publisher) = layer.publisher.as_mut()
        && let Err(e) = publisher.publish((&layer.texture, D3D12_RESOURCE_STATE_COMMON), (&layer.texture, D3D12_RESOURCE_STATE_COMMON), monaka_channel::tick())
    {
        monaka_producer::log_first!(3, "hud layer probe: layer not published: {e}");
    }
}

/// Prepares the queue hook; the command lists follow from its first execution. With `redirect`
/// the HUD draws go onto the layer only, so the frame comes out without its HUD (for a producer
/// that lays the layer over the eyes itself); else onto both. `channel` is the producer's, for
/// the probe's layer channel beside it.
///
/// # Safety
/// `queue` must be the game's live direct queue (a Streamline proxy or the real one).
pub unsafe fn install(hooks: &mut Hooks, queue: &ID3D12CommandQueue, redirect: bool, probe: bool, channel: Option<ChannelName>) -> monaka_hook::Result<()> {
    PROBING.store(probe, Relaxed);
    REDIRECT.store(redirect, Relaxed);
    REMOVED.store(false, Relaxed);
    *PROBE_CHANNEL.lock().unwrap_or_else(|e| e.into_inner()) = channel;
    // SAFETY: the caller's guarantee; the detour has the method's type.
    unsafe {
        hooks.method_noted(&EXECUTE, "ExecuteCommandLists", queue.as_raw(), slot::QUEUE_EXECUTE_COMMAND_LISTS, execute_command_lists as ExecuteFn, |why| {
            log!("hud layer: ExecuteCommandLists: {why}; hooking its vtable slot instead")
        })
    }?;
    Ok(())
}

/// Removes the command-list hooks (the queue hook goes with the producer's own, and hooks no more)
/// and lets go of the layer once the GPU is done with it.
pub fn remove(idle_ms: u64) {
    REMOVED.store(true, Relaxed);
    LAYER_VIEW.store(0, Relaxed);
    HUD_TARGET.store(0, Relaxed);
    if let Some(mut hooks) = LIST_HOOKS.lock().unwrap_or_else(|e| e.into_inner()).take() {
        if let Err(e) = hooks.disable() {
            log!("hud layer: command list hooks left in place: {e}");
        }
        InFlight::wait_idle(idle_ms);
    }
    LIST_HOOKED.store(false, Relaxed);
    if let Some(layer) = LAYER.lock().unwrap_or_else(|e| e.into_inner()).take() {
        layer.recorder.idle(2000);
    }
    log!(
        "hud layer: {} frames, {} HUD draws onto the layer; calls: events {}, targets {}, draws {}, executes {}",
        FRAMES.load(Relaxed),
        LAYER_DRAWS_TOTAL.load(Relaxed),
        CALLS[C_EVENTS].load(Relaxed),
        CALLS[C_TARGETS].load(Relaxed),
        CALLS[C_DRAWS].load(Relaxed),
        CALLS[C_EXECUTES].load(Relaxed)
    );
}
