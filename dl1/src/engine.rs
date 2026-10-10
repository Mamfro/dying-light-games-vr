//! Dying Light 1 engine facts, valid only for the fingerprinted builds below.

use monaka_core::camera::{Mat34, Mat44};
use monaka_core::convention::{CameraConvention, ProjectionConvention};
use monaka_hook::{Instruction, mem};

/// How this engine stores cameras and projections: already the shared code's canonical form,
/// so these convert nothing, but every read and write goes through them as in any engine.
pub const CAMERA: CameraConvention = CameraConvention::DYING_LIGHT;
pub const PROJECTION: ProjectionConvention = ProjectionConvention::RIGHT_HANDED;

pub use eng_chr::ENGINE;
pub const ENGINE_SHA256: &str = "24C8BC14D65EFE282E4CDAE678D0230D71167AFA37F8611715E5084F9733B361";
pub const RENDERER: &str = eng_chr::RD3D11;
pub const RENDERER_SHA256: &str = "293748B442E3744209DED5BECA80BB5B58B292504BAAC0998B385080B331D2F7";

/// Renderer global holding the game's swapchain.
pub const SWAPCHAIN_GLOBAL: usize = 0x7BC50;

/// The view setup stage `(level, view)`: it reads the view's cached render camera, so the eye
/// camera is written there just before it runs.
pub const VIEW_SETUP: usize = 0x2B4EF0;
pub const VIEW_SETUP_PROLOGUE: [Instruction; 1] = [Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]; // mov [rsp+8], rbx

pub const SET_INVERSE_CAMERA: &str = "?SetInvCameraMatrix@IBaseCamera@@QEAAXAEBVmtx34@@@Z";
/// The game calls `SetFOV` on the player camera every update.
pub const SET_FOV: &str = "?SetFOV@IBaseCamera@@QEAAXM@Z";
pub const SET_FOV_PROLOGUE: [Instruction; 2] = [
    Instruction::plain(&[0x48, 0x8b, 0x49, 0x08]),                                    // mov rcx, [rcx+8]
    Instruction::rip_relative(&[0xf3, 0x0f, 0x59, 0x0d, 0x60, 0xc7, 0x79, 0x00], 4), // mulss xmm1, [rip+...]
];
/// The game sets the player camera from a direction every update (the head-aim probe's hook).
pub const FROM_FORWARD: &str = "?FromForwardUpPos@IBaseCamera@@UEAAXAEBVvec3@@00@Z";
pub const FROM_FORWARD_PROLOGUE: [Instruction; 2] = [
    Instruction::plain(&[0x48, 0x83, 0xec, 0x28]), // sub rsp, 0x28
    Instruction::plain(&[0x4c, 0x8b, 0x59, 0x08]), // mov r11, [rcx+8]
];
/// Head aim: the game sets the player camera
/// (`CameraFPPDI`) from `gamedll+0x343339`, once per update. The camera points at +0x50 into its
/// `PlayerFppVis` (a subobject; RTTI gives the complete object), whose +0x50 is the character,
/// `PlayerDI`. The character keeps its look angles in degrees as (yaw, pitch) pairs, pitch negative
/// down: current at +0x116c/+0x1170, targets (what input adds into) at +0x1174/+0x1178, the same
/// layout DL2's `PlayerDI_PH` has at +0xb90..+0xb9c.
pub const GAMEDLL: &str = "gamedll_x64_rwdi.dll";
pub const GAMEDLL_SHA256: &str = "76F20D27C45C0B807D2F099022AD0ABC42CA90A8ADE7F761641518AF346D26A0";

/// Vertical look input (the same converter as DL2's): every input action passes through `gamedll+0x11928d0` (binding, receivers, value, source, repeat);
/// the binding holds the action id at +0 and flags at +0x10, flag 0x10 meaning the converter
/// applies `1 - value` (so the neutral value is 1). The action table entries (name pointer, two
/// lengths, id at +0x10) are checked by name before the ids are trusted.
pub const INPUT_ACTION: usize = 0x11928d0;
pub const INPUT_ACTION_PROLOGUE: [Instruction; 1] = [Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x18])]; // mov [rsp+0x18], rbx
pub const LOOK_ACTIONS: [(usize, &str, u32); 4] = [
    (0x1cde7e8, "_ACTION_LOOK_UP", 0x00),
    (0x1cde800, "_ACTION_LOOK_DOWN", 0x01),
    (0x1cdeba8, "_ACTION_ROTATE_UP", 0x32),
    (0x1cdebc0, "_ACTION_ROTATE_DOWN", 0x33),
];
pub type InputActionFn = unsafe extern "system" fn(*mut core::ffi::c_void, *mut core::ffi::c_void, f32, u8, u8);
/// The player camera update's call (head aim, above [`GAMEDLL`]): its return address. The Beast's
/// `PLAYER_CAMERA_CALL` is the call instruction itself, its return address `PLAYER_CAMERA_UPDATE`.
pub const PLAYER_CAMERA_CALL: usize = 0x343339;
/// `bool IModelObject::IsObjectOnSomeMovie()`: whether a cutscene (a "movie") has the object.
pub const IS_OBJECT_ON_SOME_MOVIE: &str = "?IsObjectOnSomeMovie@IModelObject@@QEAA_NXZ";
pub type IsOnMovieFn = unsafe extern "system" fn(*mut core::ffi::c_void) -> bool;
/// `IControlObject::GetWorldXform() const -> const mtx34&` and `SetWorldXform(const mtx34&)`.
pub const GET_WORLD_XFORM: &str = "?GetWorldXform@IControlObject@@QEBAAEBVmtx34@@XZ";
pub const SET_WORLD_XFORM: &str = "?SetWorldXform@IControlObject@@QEAAXAEBVmtx34@@@Z";
pub type GetWorldXformFn = unsafe extern "system" fn(*const core::ffi::c_void) -> *const f32;
pub type SetWorldXformFn = unsafe extern "system" fn(*mut core::ffi::c_void, *const f32);
/// The `IControlObject` inside a `PlayerDI` (its RTTI: `ControlObject` at +0x18).
pub const PLAYER_CONTROL_OBJECT: usize = 0x18;
pub const CAMERA_FPP_VIS: usize = 0x50;
pub const FPP_VIS_CHARACTER: usize = 0x50;
pub const CHARACTER_CLASS: &str = ".?AVPlayerDI@@";
pub const TARGET_YAW: usize = 0x1174;
pub const TARGET_PITCH: usize = 0x1178;
pub const CURRENT_PITCH: usize = 0x1170;
/// Video settings: `IGame::GetScreenWidth` reads `[[game+8]+0xC0]+0`. That object holds the applied mode at +0x00 and the requested one at +0x18 (each: width, height,
/// bits, fullscreen byte at +0xc, ... 0x18 bytes; engine+0x23fe80 compares them). The game's tick
/// (engine+0x22ca40 → +0x23fd40) runs `CVideoSettings::ApplyChanges` (engine+0x23ef50) when the
/// pending byte +0x3f is set; the force byte +0x88 makes it reset the device even when the two
/// modes look equal. Both bytes are cleared by the game. The game then saves `video.scr`.
pub const VIDEO_SETTINGS_IN_GAME: usize = 0xC0;
pub const VIDEO_APPLIED: usize = 0x00;
pub const VIDEO_REQUESTED: usize = 0x18;
pub const VIDEO_MODE_SIZE: usize = 0x18;
pub const VIDEO_APPLY_PENDING: usize = 0x3f;
pub const VIDEO_FORCE_APPLY: usize = 0x88;
// The game object comes from the game DLL's calls of `IGame::GetGameTimeDelta` (no exported global
// holds it): `eng_chr::game`.
/// How far the player camera moves with its pitch (degrees, positive up): metres along its level
/// heading and up, from pitch 0, every 5 degrees from -60 to 60. The character leans: looking 60
/// degrees up puts the camera 0.37 m back. The values are for the character standing still.
const CAMERA_LEAN: [(f32, f32); 25] = [
    (0.172, -0.116), (0.165, -0.105), (0.155, -0.090), (0.144, -0.077), (0.130, -0.066), (0.115, -0.055), (0.100, -0.046),
    (0.085, -0.035), (0.069, -0.026), (0.053, -0.018), (0.036, -0.011), (0.018, -0.005), (0.0, 0.0), (-0.035, 0.0),
    (-0.068, 0.001), (-0.100, -0.001), (-0.134, -0.006), (-0.167, -0.011), (-0.199, -0.019), (-0.231, -0.030),
    (-0.263, -0.046), (-0.293, -0.060), (-0.323, -0.076), (-0.351, -0.095), (-0.372, -0.110),
];

/// The camera's lean at `pitch` degrees (see [`CAMERA_LEAN`]): (forward, up) in metres,
/// interpolated, carried on along the end slopes up to 85 degrees.
pub fn camera_lean(pitch: f32) -> (f32, f32) {
    if !pitch.is_finite() {
        return (0.0, 0.0);
    }
    let at = (pitch.clamp(-85.0, 85.0) + 60.0) / 5.0;
    let i = (at.floor() as isize).clamp(0, CAMERA_LEAN.len() as isize - 2) as usize;
    let t = at - i as f32;
    let (a, b) = (CAMERA_LEAN[i], CAMERA_LEAN[i + 1]);
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

// The levels' UI after a size change: the game DLL's
// resolution-change handler, `gamedll+0x11442c0` (virtual 128, +0x400, of its `Level`, `LevelDI`,
// `GameMenuModuleDI`, `MainMenuModule` and loading modules, each an `ILevel` whose engine `CLevel`
// keeps it at +0xA8), notes where the level's UI screens are, has the level's UI manager make its
// projection again (`UI_ON_RESOLUTION_CHANGE`, the only caller of that export), runs five more of
// the level's virtuals and puts the screens back. The game does not run it when the video settings
// change size. Run from outside, at the game's frame, it breaks the game (placeholder text on the
// start screen, then access violations in the engine), so `output::video` makes only the
// projection call on each level.

/// A level's UI system, `[level + 0x568]` (the level an `ILevel` keeps at +8), and its UI manager at
/// +0x1770, as `ILevel::GetIUIManager` reads it.
pub const LEVEL_UI_HOLDER: usize = 0x568;
pub const UI_HOLDER_MANAGER: usize = 0x1770;
/// The UI system's auto layout: its update (engine+0x5dad30, from the level's update and render)
/// checks `holder + 0x14C8` every frame (engine+0x580840) and places the screens' elements against
/// the screen's edges again (each `.xui` element's `Anchor`) only when its bit 0 is set or its set
/// of screens changed. It is set only at the stop, after the camera is made for the monitor: there
/// it re-places what VR left placed for the square (such as the pause menu's SELECT hint, cut on
/// the left). Set at the start, with the menus' camera still for the old size, it makes the pause
/// menu worse.
pub const UI_HOLDER_AUTO_LAYOUT: usize = 0x14C8;
pub const AUTO_LAYOUT_DIRTY: u8 = 1;
/// `void IUIManager::OnResolutionChange()`: the manager's two layers get their projection again,
/// from the size in the video settings (`[[game + 8] + 0xC0]`, [`VIDEO_SETTINGS_IN_GAME`]).
pub const UI_ON_RESOLUTION_CHANGE: &str = "?OnResolutionChange@IUIManager@@QEAAXXZ";
// A level's game object (the game DLL's `ILevel`) is `[level + 0xA8]`, keeping the level at +8;
// whether a level's UI shows, bit 0 of its UI system's +0x2E0. The menus are levels of the game
// DLL's `MainMenuModuleDI` (the pause menu, map and inventory: made at the pause, gone at the
// resume) and `GameMenuModuleDI`.
/// The UI's own screen size (read by `IGame::GetScreenResolutionScale` through engine+0x5db980):
/// element sizes, positions and font scales come from it, each as width/1280 or
/// height/720 of the 1280x720 `.xui` design (the two disagree on a square screen, so pages
/// overflow or shift). It is `i32` width/height at `[engine + UI_SIZE_SOURCE] + 0xC8/0xCC`, unless
/// the byte at `UI_SIZE_OVERRIDDEN` is set: then the two `f32` at `UI_SIZE_OVERRIDE` (width, height).
/// The engine sets that override to its design size while it saves UI packs (engine+0x5d90f0).
/// The camera (`UI_ON_RESOLUTION_CHANGE`) reads the video settings' size instead.
pub const UI_SIZE_OVERRIDDEN: usize = 0xA3FF4B;
pub const UI_SIZE_OVERRIDE: usize = 0xABDF28;
pub type UiOnResolutionChangeFn = unsafe extern "system" fn(*mut core::ffi::c_void);
/// The game's levels (as `IGame::FreezeTimersOnLevels` and `SaveLevelsTimersState` walk them): an
/// array of level pointers at `[[game + 8] + 0xE0]`, its length an
/// `i32` at `[game + 8] + 0xE8`.
pub const GAME_LEVELS: usize = 0xE0;
pub const GAME_LEVEL_COUNT: usize = 0xE8;

/// The first-person arms: `PlayerFppVis` is the camera's target (its
/// `ICameraTarget` base sits at +0x2cb0; the player camera's +0x50 points into it). The camera
/// calls the target's slot 9, a thunk to `gamedll+0xc24160 (vis, camera)`, which post-processes a
/// few element (bone) world matrices against that camera: the root scaled toward the camera, and
/// for some weapon kinds two elements given a field-of-view correction. The arms model comes from
/// the `FakeModelObject` base at +0x40 (its virtual +0x10, as the game calls it there).
pub const FPP_CAMERA_TARGET: usize = 0xc24160;
pub const FPP_CAMERA_TARGET_PROLOGUE: [Instruction; 1] = [Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x18])]; // mov [rsp+0x18], rbx
pub const FPP_VIS_CLASS: &str = ".?AVPlayerFppVis@@";
pub const FPP_VIS_MODEL_HOLDER: usize = 0x40;
pub const MODEL_HOLDER_GET_MODEL: usize = 0x10;
/// The vis's element-index table and the indices the callback adjusts in it.
pub const FPP_VIS_ELEMENTS: usize = 0x70;
/// The vis's two weapon visuals (pointers; the callback hands each its field of view). With a gun
/// drawn the first is a `FireWeaponVis` and the second empty; melee weapons are the base
/// `WeaponVis` by the class hierarchy.
pub const FPP_VIS_WEAPONS: usize = 0x1020;
pub const FPP_ELEMENT_SLOTS: [(usize, &str); 3] = [(0xd4, "root (scaled)"), (0x41c, "fov-corrected A"), (0x29c, "fov-corrected B")];
pub type CameraTargetFn = unsafe extern "system" fn(*mut core::ffi::c_void, *mut core::ffi::c_void);
/// `IModelObject` element access, exported by the engine. Each element keeps a world matrix and a
/// local one with dirty flags: setting a world matrix marks its children for recomputation from it.
pub const ELEMENTS_NUMBER: &str = "?GetElementsNumber@IModelObject@@QEBAHXZ";
pub const ELEMENT_NAME: &str = "?GetElementNameCStr@IModelObject@@QEBAPEBDH@Z";
pub const ELEMENT_PARENT: &str = "?GetElementParent@IModelObject@@QEBAHH@Z";
pub const ELEMENT_WORLD: &str = "?GetElementWorldMatrix@IModelObject@@QEBAAEBVmtx34@@H@Z";
pub const SET_ELEMENT_WORLD: &str = "?SetElementWorldMatrix@IModelObject@@QEAAXHAEBVmtx34@@@Z";
/// Skeleton bones, as opposed to the mesh parts (`player_6_handr_fpp`, ...) skinned against them.
pub const ELEMENT_IS_BONE: &str = "?IsElementABone@IModelObject@@QEAA_NH@Z";
/// Puts an element's descendants back on the model's reference frame (its rest pose: the skinned
/// mesh's bind pose), the element itself left as it is. The engine exports no getter for the
/// reference frame; this is how it is read.
pub const RESET_DESCENDANTS: &str = "?ResetElementsDescendantsToReferenceFrame@IModelObject@@QEAAXH@Z";
pub type ResetDescendantsFn = unsafe extern "system" fn(*mut core::ffi::c_void, i32);
/// An element's matrix against its parent (the reset writes these; the world matrices follow
/// only when the engine next updates the model).
pub const ELEMENT_LOCAL: &str = "?GetElementLocalMatrix@IModelObject@@QEBAAEBVmtx34@@H@Z";
pub const SET_ELEMENT_LOCAL: &str = "?SetElementLocalMatrix@IModelObject@@QEAAXHAEBVmtx34@@@Z";
pub type ElementLocalFn = unsafe extern "system" fn(*mut core::ffi::c_void, i32) -> *const f32;
pub type SetElementLocalFn = unsafe extern "system" fn(*mut core::ffi::c_void, i32, *const f32);
pub type ElementsNumberFn = unsafe extern "system" fn(*mut core::ffi::c_void) -> i32;
pub type ElementNameFn = unsafe extern "system" fn(*mut core::ffi::c_void, i32) -> *const u8;
pub type ElementParentFn = unsafe extern "system" fn(*mut core::ffi::c_void, i32) -> i32;
pub type ElementWorldFn = unsafe extern "system" fn(*mut core::ffi::c_void, i32) -> *const f32;
pub type SetElementWorldFn = unsafe extern "system" fn(*mut core::ffi::c_void, i32, *const f32);
pub type ElementIsBoneFn = unsafe extern "system" fn(*mut core::ffi::c_void, i32) -> bool;

// Gamepads: the engine reads XInput itself (`CXPadDevice`, imports #2-#4 of XINPUT1_3, which in
// this folder is the startup proxy), through the Steam overlay's hooks; its SDL 2.0.3 also reads
// them, but only for events gameplay ignores. `monaka_pad::install` covers both.

/// Present only as a check that the level variable store is the one these offsets describe.
pub const MESH_CULL_SETTERS: [&str; 2] = ["?SetMeshCullSize@ILevel@@QEAAXM@Z", "?SetHSMMeshCullSize@ILevel@@QEAAXM@Z"];

/// Level view: the live player camera's state, the cached render camera's state, and the
/// raster-occlusion switch (the byte `SetForceViewNoRastOcclusion` writes).
pub const VIEW_LIVE_STATE: usize = 0x90390;
pub const VIEW_RENDER_STATE: usize = 0x90398;
pub const VIEW_NO_RASTER_OCCLUSION: usize = 0x92290;

/// Camera interface (`IBaseCamera`) to its state, and the state back to its interface.
pub const INTERFACE_STATE: usize = 0x08;
pub const STATE_INTERFACE: usize = 0x2B0;
/// Camera state fields: view, camera-to-world and projection matrices; near-plane extents (left,
/// right, bottom, top) with the near distance after them, which the engine derives the frustum
/// from (a written projection alone is ignored); and the detail zoom (zoom, zoom squared and
/// their reciprocals).
pub const STATE_VIEW: usize = 0x10;
pub const STATE_INVERSE: usize = 0x40;
pub const STATE_PROJECTION: usize = 0x70;
pub const STATE_EXTENTS: usize = 0x150;
pub const STATE_NEAR: usize = 0x160;
pub const STATE_ZOOM: usize = 0x230;

/// Level script variables: the variable store and the handles of `f_engine_mesh_cull_size` and
/// `f_engine_shadow_mesh_cull_size`. The store has two buffers picked by thread kind; the
/// exported setters write only one, which the engine overwrites from the other every frame, so
/// both are written.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_lean_follows_the_table() {
        assert_eq!(camera_lean(0.0), (0.0, 0.0));
        assert_eq!(camera_lean(60.0), (-0.372, -0.110));
        let (f, u) = camera_lean(2.5);
        assert!((f + 0.0175).abs() < 1e-6 && u.abs() < 1e-6, "halfway to 5 degrees");
        let (beyond, _) = camera_lean(70.0);
        assert!((beyond - (-0.372 + 2.0 * (-0.372 + 0.351))).abs() < 1e-5, "end slope carried on");
        assert_eq!(camera_lean(90.0), camera_lean(85.0));
        assert_eq!(camera_lean(f32::NAN), (0.0, 0.0));
    }
}
