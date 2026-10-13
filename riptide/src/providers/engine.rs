//! Provider: the engine facts (addresses, layouts, conventions) the other providers read.
//!
//! Dead Island Riptide Definitive Edition engine facts, valid only for the fingerprinted builds
//! below. The game runs on Dying Light 1's Chrome Engine 6, built in 2016 with an older compiler:
//! the engine's layouts are Dying Light 1's, its code is not, so every address here is this
//! build's own.

use monaka_core::camera::{Mat34, Mat44};
use monaka_core::convention::{CameraConvention, ProjectionConvention};
use monaka_hook::{Instruction, Site, mem};

/// A hook site of this build at `rva` in its module.
const fn site<F>(name: &'static str, rva: usize, prologue: &'static [Instruction]) -> Site<F> {
    // SAFETY: every site this file declares is the start of a function of the type it is declared
    // with, in the build its module's fingerprint names, its prologue that function's first
    // instructions; the bytes are checked again before a hook goes in.
    unsafe { Site::new(name, rva, prologue) }
}

/// A hook site of this build found by the name its module exports it by.
const fn exported<F>(name: &'static str, export: &'static str, prologue: &'static [Instruction]) -> Site<F> {
    // SAFETY: as [`site`]'s.
    unsafe { Site::exported(name, export, prologue) }
}

/// How this engine stores cameras and projections: Dying Light 1's, already the shared code's
/// canonical form, so these convert nothing, but every read and write goes through them.
pub const CAMERA: CameraConvention = CameraConvention::DYING_LIGHT;
pub const PROJECTION: ProjectionConvention = ProjectionConvention::RIGHT_HANDED;

pub const ENGINE_SHA256: &str = "0BCCA29EAAD957A3E6397D54FBE844878A6F9EE1CF7553A9F58AE08E3DE00F3B";
pub const RENDERER: &str = eng_chr::RD3D11;
pub const RENDERER_SHA256: &str = "5F82D55E2CFB119D32E6FB81304117A28439E1EDF443AC4BADC8380460220634";
pub const GAMEDLL: &str = "gamedll_x64_rwdi.dll";
pub const GAMEDLL_SHA256: &str = "12349061543CB3376F42AD3D450161BE1F87837BD4759403C965BA83C60FD18D";

/// Renderer global holding the game's swapchain (its window-size handler resizes it there).
pub const SWAPCHAIN_GLOBAL: usize = 0x74A40;

/// The view setup stage, the level preparing its views (`view`): it copies the view's cached render
/// camera into the scene's and the effects' cameras, so the eye camera is written there just
/// before it runs.
pub const VIEW_SETUP: Site<ViewStageFn> = site("view setup", 0x2A4890, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]); // mov [rsp+8], rbx

pub const SET_INVERSE_CAMERA: &str = "?SetInvCameraMatrix@IBaseCamera@@QEAAXAEBVmtx34@@@Z";
/// The game calls `SetFOV` on the player camera every update.
pub const SET_FOV: Site<SetFovFn> = exported(
    "SetFOV",
    "?SetFOV@IBaseCamera@@QEAAXM@Z",
    &[
        Instruction::plain(&[0x48, 0x8b, 0x49, 0x08]),                                    // mov rcx, [rcx+8]
        Instruction::rip_relative(&[0xf3, 0x0f, 0x59, 0x0d, 0xf8, 0x88, 0x7b, 0x00], 4), // mulss xmm1, [rip+...]
    ],
);
/// The game sets the player camera from a direction every update (where head aim steers).
pub const FROM_FORWARD: Site<FromForwardFn> = exported(
    "FromForwardUpPos",
    "?FromForwardUpPos@IBaseCamera@@UEAAXAEBVvec3@@00@Z",
    &[
        Instruction::plain(&[0x40, 0x53]),             // push rbx
        Instruction::plain(&[0x48, 0x83, 0xec, 0x20]), // sub rsp, 0x20
        Instruction::plain(&[0x48, 0x8b, 0x59, 0x08]), // mov rbx, [rcx+8]
    ],
);
/// Head aim: the player camera (`CameraFPPDI`) sets itself from its target's look once per update
/// (`CameraFPPDI::Update`, its vtable slot 9, [`PLAYER_CAMERA_CALL`]). The camera's target
/// (`ICameraTarget`, at +0x60) is a subobject of the `PlayerFppVis` (RTTI gives the complete
/// object), whose +0x60 is its player, the character, `PlayerDI`. The character keeps its look
/// angles in degrees as (yaw, pitch) pairs, pitch negative down: the current look at +0xc7c/+0xc80,
/// the wanted look (what input adds into) at +0xc84/+0xc88 (Dying Light 1: +0x1174/+0x1178; the
/// movement controllers' look interface reads them there in both).
///
/// `CameraFPPDI::Update`'s call of `FromForwardUpPos` on its own camera: its return address.
pub const PLAYER_CAMERA_CALL: usize = 0x8F791;
pub const CAMERA_FPP_VIS: usize = 0x60;
pub const FPP_VIS_CHARACTER: usize = 0x60;
pub const CHARACTER_CLASS: &str = ".?AVPlayerDI@@";
pub const TARGET_YAW: usize = 0xC84;
pub const TARGET_PITCH: usize = 0xC88;

// Gamepads: the engine reads XInput itself (imports of XINPUT1_3), and its SDL2 too.
// `monaka_pad::install` covers both.

/// Present only as a check that the level variable store is the one these offsets describe.
pub const MESH_CULL_SETTERS: [&str; 2] = ["?SetMeshCullSize@ILevel@@QEAAXM@Z", "?SetHSMMeshCullSize@ILevel@@QEAAXM@Z"];

/// Level view: the live player camera's state (`ILevel::GetViewCamera` reads its interface back
/// from it), the cached render camera's state (what the level view's render camera getter
/// returns), and the raster-occlusion switch (the byte `ILevel::SetForceViewNoRastOcclusion` writes).
/// All three as in Dying Light 1.
pub const VIEW_LIVE_STATE: usize = 0x90390;
pub const VIEW_RENDER_STATE: usize = 0x90398;
pub const VIEW_NO_RASTER_OCCLUSION: usize = 0x92290;

/// Camera interface (`IBaseCamera`) to its state, and the state back to its interface.
pub const INTERFACE_STATE: usize = 0x08;
pub const STATE_INTERFACE: usize = 0x2B0;
/// Camera state fields: view, camera-to-world and projection matrices; near-plane extents (left,
/// right, bottom, top) with the near distance after them, which the engine derives the frustum
/// from (a written projection alone is ignored); and the detail zoom (zoom, zoom squared and
/// their reciprocals). As in Dying Light 1.
pub const STATE_VIEW: usize = 0x10;
pub const STATE_INVERSE: usize = 0x40;
pub const STATE_PROJECTION: usize = 0x70;
pub const STATE_EXTENTS: usize = 0x150;
pub const STATE_NEAR: usize = 0x160;
pub const STATE_ZOOM: usize = 0x230;

/// Level script variables: the variable store and the handles of `f_engine_mesh_cull_size` and
/// `f_engine_shadow_mesh_cull_size`. The store has two buffers picked by thread kind; the
/// exported setters write only one, which the engine overwrites from the other every frame, so
/// both are written. As in Dying Light 1.
pub const LEVEL_VARIABLES: usize = 0xF0;
pub const MESH_CULL: usize = 0x300;
pub const SHADOW_MESH_CULL: usize = 0x310;
const VARIABLE_BUFFERS: [usize; 2] = [0x10, 0x38];

pub type SetInverseFn = unsafe extern "system" fn(*mut core::ffi::c_void, *const f32);
pub type SetFovFn = unsafe extern "system" fn(*mut core::ffi::c_void, f32);
pub type ViewStageFn = unsafe extern "system" fn(*mut core::ffi::c_void, *mut core::ffi::c_void);
pub type FromForwardFn = unsafe extern "system" fn(*mut core::ffi::c_void, *const f32, *const f32, *const f32);

/// The projection the renderer's depth buffer was written with. The camera state keeps a standard
/// infinite projection (depth = 1 - near / distance), but the renderer writes reverse-Z infinite
/// depth, `near / distance`, so the depth terms are replaced; the x/y terms stay.
pub fn depth_buffer_projection(projection: &Mat44, near: f32) -> Mat44 {
    let mut p = *projection;
    p[10] = 0.0;
    p[11] = near;
    p[14] = -1.0;
    p[15] = 0.0;
    p
}

/// One camera state, in canonical form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// World-to-camera. With an identity convention it is the engine's own, so a camera the
    /// engine left half-written fails the mutual-inverse check with `inverse`.
    pub view: Mat34,
    pub inverse: Mat34,
    pub projection: Mat44,
}

impl Camera {
    pub fn read(state: usize) -> Option<Self> {
        let inverse: Mat34 = mem::read(state + STATE_INVERSE)?;
        let projection: Mat44 = mem::read(state + STATE_PROJECTION)?;
        Some(Self { view: mem::read(state + STATE_VIEW)?, inverse: CAMERA.to_canonical(&inverse)?, projection: PROJECTION.to_canonical(&projection) })
    }
}

/// Writes a canonical projection into a camera state.
pub fn write_projection(state: usize, projection: &Mat44) -> bool {
    mem::write(state + STATE_PROJECTION, PROJECTION.from_canonical(projection))
}

fn engine_camera(camera: &Mat34) -> Mat34 {
    let mut out = [0.0; 12];
    CAMERA.from_canonical(camera, &mut out);
    out
}

/// The live player camera of a view: its interface, its state and the camera, if the state's
/// back pointer agrees.
pub fn live_camera(view: usize) -> Option<(usize, usize, Camera)> {
    let state = mem::read::<usize>(view + VIEW_LIVE_STATE)?;
    let interface = mem::read::<usize>(state + STATE_INTERFACE)?;
    (mem::read::<usize>(interface + INTERFACE_STATE)? == state).then_some(())?;
    Some((interface, state, Camera::read(state)?))
}

/// Calls the engine's `SetInvCameraMatrix`, which also rebuilds the view matrix and frustum.
pub struct CameraWriter(pub SetInverseFn);

impl CameraWriter {
    /// Sets a canonical camera-to-world matrix on a live camera interface.
    pub fn on_interface(&self, interface: usize, camera: &Mat34) {
        let matrix = engine_camera(camera);
        // SAFETY: `interface` is a live IBaseCamera read back from its own state.
        unsafe { (self.0)(interface as *mut _, matrix.as_ptr()) }
    }

    /// The function reads only `interface+8`, so a two-slot stand-in addresses a bare state such
    /// as the view's cached render camera.
    pub fn on_state(&self, state: usize, camera: &Mat34) {
        let matrix = engine_camera(camera);
        let stand_in: [usize; 2] = [0, state];
        // SAFETY: the stand-in lives across the call and holds a readable camera state.
        unsafe { (self.0)(stand_in.as_ptr() as *mut _, matrix.as_ptr()) }
    }
}

fn variable_address(level: usize, handle: usize, buffer: usize) -> Option<usize> {
    let store = mem::read::<usize>(level + LEVEL_VARIABLES)?;
    let handle = mem::read::<usize>(level + handle)?;
    let offset = mem::read::<u32>(handle + 0x50)?;
    let base = mem::read::<usize>(store + buffer)?;
    let size = mem::read::<u32>(store + buffer + 8)?;
    (offset.checked_add(4)? <= size).then_some(base + offset as usize)
}

pub fn read_level_float(level: usize, handle: usize) -> Option<f32> {
    mem::read(variable_address(level, handle, VARIABLE_BUFFERS[0])?)
}

pub fn write_level_float(level: usize, handle: usize, value: f32) -> bool {
    let mut written = false;
    for buffer in VARIABLE_BUFFERS {
        if let Some(address) = variable_address(level, handle, buffer) {
            written |= mem::write(address, value);
        }
    }
    written
}
