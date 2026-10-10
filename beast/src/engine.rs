//! Every fact about this Dying Light: The Beast build (Steam): module
//! fingerprints, function addresses with their exact first instructions, and structure offsets.
//! The Beast moved the renderer out of the engine into `renderer_x64_rwdi.dll`, which exports
//! `CCamera` by name; the engine's `IBaseCamera` exports are thunks into it
//! (`mov rcx, [rcx+0x38]; jmp [import]`). Every prologue is the shipped DLL's own.

use monaka_hook::Instruction;

pub const RENDERER: &str = "renderer_x64_rwdi.dll";
pub const RENDERER_SHA256: &str = "139ED6DD8B5BFC6A4E1D32722880F7A6557AFCA14083F0331C2A49507EC2059A";
pub use eng_chr::GAMEDLL;
pub const GAMEDLL_SHA256: &str = "DDB68C8F87BA2AFD0E561B2D1ADB9235467C29069FB1CD1619DB81E1056378EB";

/// The player camera update: the game sets its player camera once per frame on the game thread
/// with a virtual call of the engine's `IBaseCamera::FromForwardUpPos` (`call [rax+0x5e8]`),
/// which lands in `CCamera::SetView(vec3, vec3, vec3)`. Its "forward" is the camera's backward
/// axis (the game negates it first), as in DL2. The return address, and the call before it.
pub const PLAYER_CAMERA_UPDATE: usize = 0x1510617;
pub const PLAYER_CAMERA_CALL: (usize, &[u8]) = (0x1510611, &[0xff, 0x90, 0xe8, 0x05, 0x00, 0x00]);

/// A hook target: its export name, RVA (checked against the export) and first instructions.
pub type Target = (&'static str, usize, &'static [Instruction]);

/// `CCamera::SetView(const mtx34& cameraToWorld, bool)`, DL2's camera component setter.
pub type SetMatrixFn = unsafe extern "system" fn(camera: usize, matrix: usize, flag: usize);
pub const SET_VIEW_MATRIX: Target = (
    "?SetView@CCamera@@UEAAXAEBVmtx34@@_N@Z",
    0x7620,
    &[Instruction::plain(&[0x48, 0x81, 0xec, 0x88, 0x00, 0x00, 0x00])], // sub rsp, 0x88
);
/// `CCamera::SetView(const vec3& forward, const vec3& up, const vec3& position)`, behind the
/// engine's `IBaseCamera::FromForwardUpPos`.
pub type SetVectorsFn = unsafe extern "system" fn(camera: usize, forward: usize, up: usize, position: usize);
pub const SET_VIEW_VECTORS: Target = (
    "?SetView@CCamera@@QEAAXAEBVvec3@@00@Z",
    0x7210,
    &[Instruction::plain(&[0x48, 0x8b, 0xc4]), Instruction::plain(&[0x48, 0x89, 0x58, 0x08])], // mov rax, rsp; mov [rax+8], rbx
);
/// `CShaderCamera::GetFrameJitter(frame)`, static, returning the sub-pixel jitter through a hidden
/// pointer: every jitter use goes through it (the projection's `TaaJitter` and the DLSS packet).
pub type FrameJitterFn = unsafe extern "system" fn(out: *mut [f32; 2], frame: u64) -> *mut [f32; 2];
pub const FRAME_JITTER: Target = (
    "?GetFrameJitter@CShaderCamera@@SA?BVvec2@@_K@Z",
    0x61a0,
    &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])], // mov [rsp+8], rbx
);
/// `CCamera::ComputeFrustumMatrix(bool)`, DL2's projection rebuild.
pub type ComputeFrustumFn = unsafe extern "system" fn(camera: usize, flag: usize);
pub const COMPUTE_FRUSTUM: Target = (
    "?ComputeFrustumMatrix@CCamera@@IEAAX_N@Z",
    0x51d0,
    &[Instruction::plain(&[0x4c, 0x8b, 0xdc]), Instruction::plain(&[0x49, 0x89, 0x4b, 0x08])], // mov r11, rsp; mov [r11+8], rcx
);

// --- D3D12 renderer ------------------------------------------------------------------------------
pub const RENDERER_12: &str = eng_chr::RD3D12;
pub const RENDERER_12_SHA256: &str = "68BCDE0658BA0E93F5CA448C40AB10C1165A3B748CA06D50F3EF85239DDF2530";
/// The present request (DL2's `PRESENT_REQUEST_12`; the function that reports
/// `"hr = m_SwapChain4->Present1(interval, flags, &params)"`).
pub use eng_chr::rd3d12::PresentRequestFn as PresentRequest12Fn;
pub const PRESENT_REQUEST_12: (usize, &[Instruction]) = (
    0x76450,
    &[Instruction::plain(&[0x40, 0x53]), Instruction::plain(&[0x48, 0x81, 0xec, 0x70, 0x08, 0x00, 0x00])], // push rbx; sub rsp, 0x870
);
/// The present request presents when the object's +0x60 is non-zero, through the swapchain at +0x70
/// (both 0x10 further than in DL2). The game's direct queue, through a holder pointer: the first of
/// the two holders the present request's other branch loads, as `0x164b50` is in DL2.
pub const RD3D12: eng_chr::rd3d12::Layout = eng_chr::rd3d12::Layout { request_active: 0x60, request_swapchain: 0x70, queue_holder: 0xe4bc0 };
/// rd3d12 looks up Streamline's `slDLSSSetOptions` once and keeps it here, calling it as
/// `(&viewport, &options)` (rd3d12+0x3fc03).
pub const DLSS_SET_OPTIONS_POINTER: usize = 0xe4eb0;

// --- Video settings (headset resolution) -----------------------------------------------------------
/// The engine (the game object comes from its `IGame::GetGameTimeDelta`: `eng_chr::game`).
pub use eng_chr::ENGINE;
/// `CVideoSettings` at `[[IGame+8]+0xd8]` (`IGame::GetScreenWidth`; DL2: +0xe0). Its
/// `SVideoSettings` is at +8 (window mode +4, width +8, height +0xc, bit depth +0x18, change mask
/// +0xf0: 1 window mode, 2 resolution); +0x7b9 and +0x7bb request an apply on the game's next
/// frame (DL2: +0x699/+0x69b), as the engine's own request does after `SetResolution` and
/// `SetWindowMode` (engine+0x835361).
pub const VIDEO_SETTINGS_IN_GAME: usize = 0xd8;
pub const VIDEO_SETTINGS_STRUCT: usize = 8;
pub const VIDEO_WINDOW_MODE: usize = 4;
pub const VIDEO_WIDTH: usize = 8;
pub const VIDEO_HEIGHT: usize = 0xc;
pub const VIDEO_BIT_DEPTH: usize = 0x18;
pub const VIDEO_APPLY_REQUESTS: [usize; 2] = [0x7b9, 0x7bb];
pub const WINDOWED: u32 = 0;
/// The renderer's own setters (they mark the change mask).
pub const SET_RESOLUTION: &str = "?SetResolution@SVideoSettings@@QEAAXIII@Z";
pub const SET_WINDOW_MODE: &str = "?SetWindowMode@SVideoSettings@@QEAAXW4TYPE@EWindowMode@@@Z";
pub type SetResolutionFn = unsafe extern "system" fn(settings: usize, width: u32, height: u32, bit_depth: u32);
pub type SetWindowModeFn = unsafe extern "system" fn(settings: usize, mode: u32);

// --- Render commands (renderer) ------------------------------------------------------------------
/// `CmdPPFX_DLSS` execute (vtable slot 1): `(command, render context)`, on the render thread while
/// the frame's render data is current (it fills the DLSS packet from it).
pub type CommandExecuteFn = unsafe extern "system" fn(command: usize, context: usize) -> usize;
pub const DLSS_COMMAND_EXECUTE: (usize, &[Instruction]) =
    (0x820e0, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08]), Instruction::plain(&[0x57])]); // mov [rsp+8], rbx; push rdi
/// The renderer object (`CRenderer`) and its frame render data while a frame renders (interface
/// method +0x218); the data's view cameras start at +0x50, the main view (index 20) at +0xf0.
pub const RENDERER_GLOBAL: usize = 0x32c8c0;
pub const RENDERER_FRAME_DATA: usize = 0x1890;

// --- CCamera (from its exported getters); the world is y up --------------------------------------
/// World-to-camera 3x4 (`GetCameraMatrix`).
pub const CAMERA_VIEW: usize = 0x10;
/// Camera-to-world 3x4 (`GetInvCameraMatrix`); the position is its translation column
/// (+0x4c, +0x5c, +0x6c, `GetPosition`).
pub const CAMERA_INVERSE: usize = 0x40;
/// Projection 4x4 (`GetProjectionMatrix`).
pub const CAMERA_PROJECTION: usize = 0x80;
/// Near-plane extents (left, right, bottom, top) the projection is built from by
/// `ComputeFrustumMatrix` (`SetFrustum` writes them in that order, then calls it).
pub const CAMERA_EXTENTS: usize = 0x1a0;
pub const CAMERA_NEAR: usize = 0x1b0;
pub const CAMERA_FAR: usize = 0x1b4;

// --- The player's look and the first-person arms (gamedll) -------------------------------------------
// The Beast's player camera update (gamedll 0x15104b0) is DL2's instruction for instruction. The
// camera's +0x40 points at the arms visual's `ICameraTarget` base (`PlayerFppVis_PH`, base at vis+0x5c0,
// vtable `FPP_VIS_VTABLE`); the character `PlayerDI_PH` sits at [target - 0x580] (vis+0x40). Its look
// angles are DL2's, 0x20 further: degrees, pitch positive up (the forward comes from
// `GetVectorFromHorzVertAngle(+0xbb0, +0xbb4)`), and the look update smooth-damps them toward the
// targets the input accumulates into. Yaw is taken to turn right as it grows, as in DL2.
pub const CAMERA_TARGET: usize = 0x40;
/// The renderer's `CCamera` behind a game camera (the engine's camera calls are thunks into it).
pub const CAMERA_RENDERER: usize = 0x38;
pub const FPP_VIS_VTABLE: usize = 0x259eda8;
pub const PLAYER_VTABLE: usize = 0x259c450;
/// The character from the camera target.
pub const TARGET_CHARACTER: usize = 0x580;
pub const LOOK_YAW: usize = 0xbb0;
pub const LOOK_PITCH: usize = 0xbb4;
pub const TARGET_YAW: usize = 0xbb8;
pub const TARGET_PITCH: usize = 0xbbc;
/// The game's input-action converter `(binding, receivers, value, source, repeat)`, DL2's: action
/// id at binding+0, flags at +0x10 (0x10: inverted, neutral 1).
pub type InputActionFn = unsafe extern "system" fn(binding: usize, receivers: usize, value: f32, source: bool, repeat: bool);
pub const INPUT_ACTION: (usize, &[Instruction]) = (0x1fae0b0, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x10])]);
/// The vertical look actions, checked in the engine's action table: (RVA of `mov edx, id`, name
/// RVA, id, name). The ids moved from DL2's (0x5e, 0x5f, 0x89, 0x8a).
pub const PITCH_ACTIONS: [(usize, usize, u32, &str); 4] = [
    (0xf87720, 0x18e8330, 0x65, "_ACTION_LOOK_UP"),
    (0xf8776e, 0x18e8340, 0x66, "_ACTION_LOOK_DOWN"),
    (0xf8843a, 0x18e8720, 0x90, "_ACTION_ROTATE_UP"),
    (0xf88488, 0x18e8738, 0x91, "_ACTION_ROTATE_DOWN"),
];
/// The arms visual's camera-target callback `(ICameraTarget base, camera)`, slot 12 of the target
/// vtable, called right after the camera is set; like DL2's it changes no bones unless the legacy
/// anti-wall byte (gamedll 0x39512b8) is set.
pub const FPP_CAMERA_TARGET: eng_chr::fpp::Callback = eng_chr::fpp::Callback {
    rva: 0x1223d10,
    prologue: &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08]), Instruction::plain(&[0x57]), Instruction::plain(&[0x48, 0x83, 0xec, 0x20])],
    vtable: FPP_VIS_VTABLE,
    slot: 0x60,
};
pub const FPP_ICAMERA_TARGET: usize = 0x5c0;
/// The arms skeleton's handle in the vis and `CoSkeleton`'s vtable slots (all moved from DL2's).
pub const SKELETON_LAYOUT: eng_chr::coskeleton::Layout =
    eng_chr::coskeleton::Layout { handle: 0x68, world_slot: 0x438, count_slot: 0x4c0, name_slot: 0x4c8, set_world_slot: 0x450 };
pub const ARMS_LAYOUT: eng_chr::fpp::Layout = eng_chr::fpp::Layout { weapons: 0x910 };
/// DL2's skeleton (its wrist half turned about x against DL1's): The Beast's idle one-handed melee
/// wrist frame matches DL2's within about a degree. The gun holder has no correction of its own,
/// as in DL2.
pub const BONE_AXES: monaka_arms::BoneAxes = monaka_arms::BoneAxes::DYING_LIGHT_2;
/// The finger bones' axes ([`eng_chr::fingers`]), as Dying Light 2's.
pub const FINGER_AXES: eng_chr::fingers::Axes = eng_chr::fingers::Axes::DYING_LIGHT_2;
// --- The engine's gui tree (the dynamic HUD; `eng_chr::gui`) ---------------------------------------------
/// The implementations the exported `gui::IElement` getters jump to: `GetWorldMatrix`'s
/// `push rbx; sub rsp, 0x20; test byte
/// [rcx+0x16a], 0x10`; `GetActualPos`'s and `GetActualSize`'s `push rbx; sub rsp, 0x20; mov rbx, rcx`.
pub const GUI_BUILD: eng_chr::gui::Build = eng_chr::gui::Build {
    world: (0xaad8f0, &[Instruction::plain(&[0x40, 0x53]), Instruction::plain(&[0x48, 0x83, 0xec, 0x20]), Instruction::plain(&[0xf6, 0x81, 0x6a, 0x01, 0x00, 0x00, 0x10])]),
    position: (0xaa4f70, &[Instruction::plain(&[0x40, 0x53]), Instruction::plain(&[0x48, 0x83, 0xec, 0x20]), Instruction::plain(&[0x48, 0x8b, 0xd9])]),
    size: (0xaa4f90, &[Instruction::plain(&[0x40, 0x53]), Instruction::plain(&[0x48, 0x83, 0xec, 0x20]), Instruction::plain(&[0x48, 0x8b, 0xd9])]),
};
/// The pieces of the HUD the dynamic HUD takes, by Dying Light 2's widget names (`probe_gui=1`
/// logs this game's documents and widgets).
pub const PIECES: [eng_chr::gui::Piece; 5] = {
    use eng_chr::gui::{Extent, Piece};
    [
        Piece { name: "minimap", document: "hud_screen_layer_radar", path: &["radar"], extent: Extent::Declared, size: (1280, 96) },
        Piece { name: "weapon", document: "hud_screen_layer_0", path: &["extendend_hud_group", "weapon_selector"], extent: Extent::Declared, size: (768, 384) },
        Piece { name: "quests", document: "hud_screen_layer_0", path: &["extendend_hud_group", "group1", "group1", "wrap_panel", "hud_objectives"], extent: Extent::Leaves, size: (1024, 352) },
        Piece { name: "health", document: "hud_screen_layer_0", path: &["extendend_hud_group", "health_buff_revive_groupped"], extent: Extent::Leaves, size: (768, 256) },
        Piece { name: "tool", document: "hud_screen_layer_0", path: &["extendend_hud_group", "stamina_bar"], extent: Extent::Leaves, size: (768, 96) },
    ]
};

// --- Physical melee and the movement step (gamedll, engine) -------------------------------------------
/// The melee code (`eng_chr::melee`): DL2's
/// at new addresses; the attack start's window at +0x1d4 and its copy (behind the controller's
/// "time until the hit window", 0xf5a7d0) at +0x530; attack types 0..=26; segments carry 16 more
/// bytes than DL2's.
pub const MELEE: eng_chr::melee::Build = eng_chr::melee::Build {
    attack_start: 0xfe5900,
    direction_getter: 0x1054110,
    test_segments: 0x1159580,
    window_start: 0x1d4,
    window_start_copy: 0x530,
    attack_types: 0x7ff_f9ff,
    segment: 0x44,
    player_vtable: PLAYER_VTABLE,
};
/// The player's movement step (`eng_chr::walk`): the module step is `PlayerBulletPhysicsModule`'s
/// vtable slot 120, `GameStep` the character interface's slot 127 (+0x3f8). The engine's build is
/// not pinned: `GameStep`'s first bytes are.
pub const WALK: eng_chr::walk::Build = eng_chr::walk::Build {
    module_step: 0xfec4e0,
    game_step: 0xf3cd70,
    game_step_prologue: Some(&[0x48, 0x8b, 0xc4, 0x4c, 0x89, 0x48, 0x20, 0x55, 0x53, 0x56, 0x48, 0x8d, 0xa8, 0x58, 0xfc, 0xff]),
    player_vtable: PLAYER_VTABLE,
    step_offset: 0x280,
    attached: 0x2cc,
};
