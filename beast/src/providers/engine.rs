//! Provider: the engine facts (addresses, layouts, conventions) the other providers read, and
//! the Chrome facts' (`crate::Beast`'s `Chrome`).
//!
//! Every fact about this Dying Light: The Beast build (Steam): module
//! fingerprints, function addresses with their exact first instructions, and structure offsets.
//! The Beast moved the renderer out of the engine into `renderer_x64_rwdi.dll`, which exports
//! `CCamera` by name; the engine's `IBaseCamera` exports are thunks into it
//! (`mov rcx, [rcx+0x38]; jmp [import]`). Every prologue is the shipped DLL's own.

use eng_chr::providers::look_fields::InputActionFn;
use eng_chr::providers::rd3d12::PresentRequestFn;
use monaka_hook::{Instruction, Site};

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

pub const RENDERER: &str = "renderer_x64_rwdi.dll";
pub const RENDERER_SHA256: &str = "139ED6DD8B5BFC6A4E1D32722880F7A6557AFCA14083F0331C2A49507EC2059A";
pub const GAMEDLL_SHA256: &str = "DDB68C8F87BA2AFD0E561B2D1ADB9235467C29069FB1CD1619DB81E1056378EB";

/// The player camera update: the game sets its player camera once per frame on the game thread
/// with a virtual call of the engine's `IBaseCamera::FromForwardUpPos` (`call [rax+0x5e8]`),
/// which lands in `CCamera::SetView(vec3, vec3, vec3)`. Its "forward" is the camera's backward
/// axis (the game negates it first), as in DL2. The return address, and the call before it.
pub const PLAYER_CAMERA_UPDATE: usize = 0x1510617;
pub const PLAYER_CAMERA_CALL: (usize, &[u8]) = (0x1510611, &[0xff, 0x90, 0xe8, 0x05, 0x00, 0x00]);

/// `CCamera::SetView(const mtx34& cameraToWorld, bool)`, DL2's camera component setter (renderer
/// RVA 0x7620).
pub type SetMatrixFn = unsafe extern "system" fn(camera: usize, matrix: usize, flag: usize);
pub const SET_VIEW_MATRIX: Site<SetMatrixFn> = exported(
    "CCamera::SetView(mtx34)",
    "?SetView@CCamera@@UEAAXAEBVmtx34@@_N@Z",
    &[Instruction::plain(&[0x48, 0x81, 0xec, 0x88, 0x00, 0x00, 0x00])], // sub rsp, 0x88
);
/// `CCamera::SetView(const vec3& forward, const vec3& up, const vec3& position)`, behind the
/// engine's `IBaseCamera::FromForwardUpPos` (renderer RVA 0x7210).
pub type SetVectorsFn = unsafe extern "system" fn(camera: usize, forward: usize, up: usize, position: usize);
pub const SET_VIEW_VECTORS: Site<SetVectorsFn> = exported(
    "CCamera::SetView(vec3)",
    "?SetView@CCamera@@QEAAXAEBVvec3@@00@Z",
    &[Instruction::plain(&[0x48, 0x8b, 0xc4]), Instruction::plain(&[0x48, 0x89, 0x58, 0x08])], // mov rax, rsp; mov [rax+8], rbx
);
/// `CShaderCamera::GetFrameJitter(frame)`, static, returning the sub-pixel jitter through a hidden
/// pointer: every jitter use goes through it (the projection's `TaaJitter` and the DLSS packet;
/// renderer RVA 0x61a0).
pub type FrameJitterFn = unsafe extern "system" fn(out: *mut [f32; 2], frame: u64) -> *mut [f32; 2];
pub const FRAME_JITTER: Site<FrameJitterFn> = exported(
    "CShaderCamera::GetFrameJitter",
    "?GetFrameJitter@CShaderCamera@@SA?BVvec2@@_K@Z",
    &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])], // mov [rsp+8], rbx
);
/// `CCamera::ComputeFrustumMatrix(bool)`, DL2's projection rebuild (renderer RVA 0x51d0).
pub type ComputeFrustumFn = unsafe extern "system" fn(camera: usize, flag: usize);
pub const COMPUTE_FRUSTUM: Site<ComputeFrustumFn> = exported(
    "CCamera::ComputeFrustumMatrix",
    "?ComputeFrustumMatrix@CCamera@@IEAAX_N@Z",
    &[Instruction::plain(&[0x4c, 0x8b, 0xdc]), Instruction::plain(&[0x49, 0x89, 0x4b, 0x08])], // mov r11, rsp; mov [r11+8], rcx
);

// --- D3D12 renderer ------------------------------------------------------------------------------
pub const RENDERER_12: &str = eng_chr::RD3D12;
pub const RENDERER_12_SHA256: &str = "68BCDE0658BA0E93F5CA448C40AB10C1165A3B748CA06D50F3EF85239DDF2530";
/// The present request (DL2's `PRESENT_REQUEST_12`; the function that reports
/// `"hr = m_SwapChain4->Present1(interval, flags, &params)"`).
pub const PRESENT_REQUEST_12: Site<PresentRequestFn> = site(
    "present request",
    0x76450,
    &[Instruction::plain(&[0x40, 0x53]), Instruction::plain(&[0x48, 0x81, 0xec, 0x70, 0x08, 0x00, 0x00])], // push rbx; sub rsp, 0x870
);
/// The present request presents when the object's +0x60 is non-zero, through the swapchain at +0x70
/// (both 0x10 further than in DL2). The game's direct queue, through a holder pointer: the first of
/// the two holders the present request's other branch loads, as `0x164b50` is in DL2.
pub const RD3D12: eng_chr::providers::rd3d12::Layout = eng_chr::providers::rd3d12::Layout { request_active: 0x60, request_swapchain: 0x70, queue_holder: 0xe4bc0 };
/// rd3d12 looks up Streamline's `slDLSSSetOptions` once and keeps it here, calling it as
/// `(&viewport, &options)` (rd3d12+0x3fc03).
pub const DLSS_SET_OPTIONS_POINTER: usize = 0xe4eb0;

// --- Video settings (headset resolution) -----------------------------------------------------------
// The engine is `eng_chr::ENGINE` (the game object comes from its `IGame::GetGameTimeDelta`:
// `eng_chr::providers::game`).
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
/// The frame data's main view camera (the one DLSS's and FSR's packets take the camera from).
pub const FRAME_MAIN_VIEW: usize = 0xf0;

// --- CCamera (from its exported getters); the world is y up --------------------------------------
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
/// Where the arms visual's camera target takes the camera's direction from, as its direction getter
/// (gamedll 0x1151450) chooses: while the byte 0x560 before the target is set, from the first of two
/// camera components on the character (type ids at 0x34109d0 and 0x341082c, found by the lookup at
/// 0x1ef3970) that says it gives the camera (its slot +0x3c0), each an animated model's bone; else,
/// while the byte 0x45c before the target is set, from an animated bone; else from the look angles.
pub const CAMERA_SOURCES: eng_chr::providers::look_fields::CameraSources = eng_chr::providers::look_fields::CameraSources {
    animated: 0x45c,
    components: 0x560,
    lookup: 0x1ef3970,
    component_ids: [0x34109d0, 0x341082c],
    gives_camera: 0x3c0,
};
/// The renderer's `CCamera` behind a game camera (the engine's camera calls are thunks into it).
pub const CAMERA_RENDERER: usize = 0x38;
pub const FPP_VIS_VTABLE: usize = 0x259eda8;
pub const PLAYER_VTABLE: usize = 0x259c450;
/// The player's look controller's update (vtable slot 143, `(controller, dt)`): in almost every
/// controller group, so it runs every frame (`crate::providers::controllers`).
pub const LOOK_UPDATE: usize = 0x1026380;
/// The player's parameter block's getter (vtable slot of `PlayerDI_PH`, `() -> block`).
pub const PARAMETERS_SLOT: usize = 0x5e0;
/// The first-person arms' anti-wall scales in the parameter block (f32 each, the arms drawn scaled
/// toward the camera by them): `AntiWallHandsScale`, `AntiWallHangHandsScale`,
/// `AntiWallAimHandsScale`, `AntiWallLowCrawlScale` (`crate::providers::anti_wall`).
pub const ANTI_WALL_SCALES: [usize; 4] = [0xb528, 0xb558, 0xb570, 0xb588];
/// The controllers the player climbs with (`controllers_setup.scr`: the groups `Ladder`, `EdgeGrab`,
/// `ZipLine`, `FreeClimb`, each controller in its own group only) and their updates (vtable slot
/// 143, `(controller, dt)`), for [`crate::providers::controllers`].
pub const CLIMBING_UPDATES: [(&str, usize); 7] = [
    ("ladder", 0x10259f0),
    ("edge", 0x1022320),
    ("hang", 0x1023720),
    ("climb", 0x1020d90),
    ("shimmy", 0x10308a0),
    ("zip line", 0x1037130),
    ("free climb", 0x10227f0),
];
/// The character from the camera target.
pub const TARGET_CHARACTER: usize = 0x580;
pub const LOOK_YAW: usize = 0xbb0;
pub const LOOK_PITCH: usize = 0xbb4;
pub const TARGET_YAW: usize = 0xbb8;
pub const TARGET_PITCH: usize = 0xbbc;
/// The game's input-action converter `(binding, receivers, value, source, repeat)`, DL2's: action
/// id at binding+0, flags at +0x10 (0x10: inverted, neutral 1).
pub const INPUT_ACTION: Site<InputActionFn> = site("input action", 0x1fae0b0, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x10])]);
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
pub const FPP_CAMERA_TARGET: eng_chr::providers::fpp::Callback = eng_chr::providers::fpp::Callback {
    rva: 0x1223d10,
    prologue: &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08]), Instruction::plain(&[0x57]), Instruction::plain(&[0x48, 0x83, 0xec, 0x20])],
    vtable: FPP_VIS_VTABLE,
    slot: 0x60,
};
pub const FPP_ICAMERA_TARGET: usize = 0x5c0;
/// The arms skeleton's handle in the vis and `CoSkeleton`'s vtable slots (all moved from DL2's).
pub const SKELETON_LAYOUT: eng_chr::providers::coskeleton::Layout =
    eng_chr::providers::coskeleton::Layout { handle: 0x68, world_slot: 0x438, count_slot: 0x4c0, name_slot: 0x4c8, set_world_slot: 0x450 };
pub const ARMS_LAYOUT: eng_chr::providers::fpp::Layout = eng_chr::providers::fpp::Layout { weapons: 0x910 };
/// DL2's skeleton (its wrist half turned about x against DL1's): The Beast's idle one-handed melee
/// wrist frame matches DL2's within about a degree. The gun holder has no correction of its own,
/// as in DL2.
pub const BONE_AXES: monaka_body::BoneAxes = monaka_body::BoneAxes::DYING_LIGHT_2;
/// How the left arm's bones mirror the right ones' (as Dying Light 1's): declared, not measured at
/// the start, which needs the arms posed alike (a run that starts crawling never is).
pub const LEFT_MIRROR: [f32; 9] = [1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0];
// --- The engine's gui tree (the dynamic HUD; `eng_chr::providers::gui`) -------------------------------
/// The implementations the exported `gui::IElement` getters jump to: `GetWorldMatrix`'s
/// `push rbx; sub rsp, 0x20; test byte
/// [rcx+0x16a], 0x10`; `GetActualPos`'s and `GetActualSize`'s `push rbx; sub rsp, 0x20; mov rbx, rcx`.
pub const GUI_BUILD: eng_chr::providers::gui::Build = eng_chr::providers::gui::Build {
    world: (0xaad8f0, &[Instruction::plain(&[0x40, 0x53]), Instruction::plain(&[0x48, 0x83, 0xec, 0x20]), Instruction::plain(&[0xf6, 0x81, 0x6a, 0x01, 0x00, 0x00, 0x10])]),
    position: (0xaa4f70, &[Instruction::plain(&[0x40, 0x53]), Instruction::plain(&[0x48, 0x83, 0xec, 0x20]), Instruction::plain(&[0x48, 0x8b, 0xd9])]),
    size: (0xaa4f90, &[Instruction::plain(&[0x40, 0x53]), Instruction::plain(&[0x48, 0x83, 0xec, 0x20]), Instruction::plain(&[0x48, 0x8b, 0xd9])]),
};
/// The pieces of the HUD the dynamic HUD takes, by this game's widget names (`probe_gui=1` logs its
/// documents and widgets), on the hands as Dying Light 1 has them: the compass (`minimap`) with
/// the quest objectives beside it on the back of the right hand, the weapon slot above its palm;
/// health on the back of the left hand, the two tool slots above its palm. Health and the three
/// slots sit together at the bottom left of the HUD (`group2`): the weapon selector's slots are
/// `left` and `up` (the tools, read together as one piece) and `right_wpn` (the weapon, its ammo
/// beside it). The slots and health are read by their own boxes: their leaves include hidden
/// groups (a key label 200 px tall, a mods group at three times the scale) that would stretch the
/// crop over the neighbouring slots; health by its bars (`group3`: health and fury). The list is
/// read by its shown leaves; the compass by its own box (its markers float about in it). Each panel
/// has its piece's shape, so a crop takes in nothing around it.
pub const PIECES: [eng_chr::providers::gui::Piece; 5] = {
    use eng_chr::providers::gui::{Extent, Piece};
    use monaka_stereo::hud_panels::Piece as Panel;
    const SLOTS: &[&str] = &["extendend_hud_group", "group2", "weapon_selector", "group1"];
    [
        Piece { panel: Panel { name: "minimap", size: (1280, 96) }, document: "hud_screen_layer_radar", path: &["radar"], parts: &[], extent: Extent::Declared },
        Piece { panel: Panel { name: "weapon", size: (384, 261) }, document: "hud_screen_layer_0", path: SLOTS, parts: &["right_wpn"], extent: Extent::Declared },
        Piece {
            panel: Panel { name: "quests", size: (1152, 480) },
            document: "hud_screen_layer_0",
            path: &["extendend_hud_group", "group3", "group1", "group1", "wrap_panel", "hud_objectives"],
            parts: &[],
            extent: Extent::Leaves,
        },
        Piece {
            panel: Panel { name: "health", size: (768, 101) },
            document: "hud_screen_layer_0",
            path: &["extendend_hud_group", "group2", "health_buff_revive_groupped"],
            parts: &["group3"],
            extent: Extent::Declared,
        },
        Piece { panel: Panel { name: "tool", size: (768, 374) }, document: "hud_screen_layer_0", path: SLOTS, parts: &["left", "up"], extent: Extent::Declared },
    ]
};
/// The pieces' panels (`monaka_stereo::hud_panels::HudPieces::PIECES`).
pub const PANELS: [monaka_stereo::hud_panels::Piece; 5] = eng_chr::providers::gui::panels(&PIECES);

/// The player's `HeadBobFactor` in its parameter block (for `eng_chr::providers::view_bob`).
pub const VIEW_BOB: eng_chr::providers::view_bob::Build = eng_chr::providers::view_bob::Build { player_vtable: PLAYER_VTABLE, parameters_slot: 0x5e0, head_bob: 0x3420 };
// --- Physical melee and the movement step (gamedll, engine) -------------------------------------------
/// The game object and the class the game's combat target search finds (for
/// `eng_chr::providers::melee`).
pub const MELEE_REACH: eng_chr::providers::melee::Targets = eng_chr::providers::melee::Targets { game: 0x3acb4f0, target_class: 0x3952268 };
/// The melee code (`eng_chr::providers::melee`): DL2's
/// at new addresses; the attack start's window at +0x1d4 and its copy (behind the controller's
/// "time until the hit window", 0xf5a7d0) at +0x530; attack types 0..=26; segments carry 16 more
/// bytes than DL2's.
pub const MELEE: eng_chr::providers::melee::Build = eng_chr::providers::melee::Build {
    attack_start: 0xfe5900,
    direction_getter: 0x1054110,
    test_segments: 0x1159580,
    window_start: 0x1d4,
    window_start_copy: 0x530,
    attack_types: 0x7ff_f9ff,
    segment: 0x44,
    player_vtable: PLAYER_VTABLE,
};
/// The sync action conditions' checks (vtable slot 1 of `SyncActionConditionIsBusy`,
/// `SyncActionConditionIsClimbing` and `SyncActionConditionMinDistance`, for
/// `eng_chr::providers::grabs`).
pub const GRABS: eng_chr::providers::grabs::Build = eng_chr::providers::grabs::Build { is_busy: 0x2160ae0, in_climb: 0x2160bc0, min_distance: 0x2160f20 };
/// The gun controller (`WeaponFireController`, for the reload taken the game's own way,
/// `eng_chr::providers::actions`), DL2's at new addresses: update slot 143, the reload start (as the
/// reload input calls it), state at +0x38; before the start the input marks the weapons that pass
/// two checks (the controller's weapon at slot +0x4a8) at +0x289.
pub const RELOAD: eng_chr::providers::actions::Reload = eng_chr::providers::actions::Reload {
    update: 0x1034e60,
    start: 0x10192b0,
    state: 0x38,
    prepare: Some(eng_chr::providers::actions::Prepare { weapon_slot: 0x4a8, checks: [0x1292a80, 0x1292ac0], flag: 0x289 }),
    player_vtable: PLAYER_VTABLE,
};
/// What the hands do with the game's own actions (`eng_chr::providers::hands::Builds`), DL2's at new
/// addresses:
/// - the throw controller: update vtable slot 143, state at +0x40, the release made once (+0x132),
///   the throwing hand at +0x138; the player's parameter block (slot +0x5e0) with
///   `ThrowParabolaEnabled` at +0x74d8;
/// - the dropped item's launch: velocity +0x7d4, thrown +0x7ed, owner +0x850;
/// - the player's aim part (player +0x1b0): eye slot +0x58, look slot +0x50;
/// - the bow controller: update slot 143, state at +0x40, the draw's power at +0x8c and its most at
///   +0x90.
///
/// The lockpick minigame's fields are not read for this build.
pub const HANDS: eng_chr::providers::hands::Builds = {
    use eng_chr::providers::hands::Builds;
    use eng_chr::providers::{aim, bow, throws};
    Builds {
        throw: Some(throws::Build { update: 0x1036360, velocity: 0xf5a1e0, start: 0xf53e50, release: 0x1007720, state: 0x40, released: 0x132, hand: 0x138, player_vtable: PLAYER_VTABLE, parameters_slot: 0x5e0, arc: 0x74d8 }),
        weapon_throw: Some(throws::WeaponBuild { launch: 0xd682c0, velocity: 0x7d4, thrown: 0x7ed, owner: 0x850, player_vtable: PLAYER_VTABLE }),
        aim: aim::Build { eye: 0x1286a30, look: 0x12867d0 },
        bow: Some(bow::Build { update: 0x1034250, arrow: 0xff8d50, state: 0x40, power: 0x8c, player_vtable: PLAYER_VTABLE }),
        lockpick: None,
    }
};
/// The player's movement step (`eng_chr::providers::walk`): the module step is
/// `PlayerBulletPhysicsModule`'s vtable slot 120, `GameStep` the character interface's slot 127
/// (+0x3f8). The engine's build is not pinned: `GameStep`'s first bytes are.
pub const WALK: eng_chr::providers::walk::Build = eng_chr::providers::walk::Build {
    module_step: 0xfec4e0,
    game_step: 0xf3cd70,
    game_step_prologue: Some(&[0x48, 0x8b, 0xc4, 0x4c, 0x89, 0x48, 0x20, 0x55, 0x53, 0x56, 0x48, 0x8d, 0xa8, 0x58, 0xfc, 0xff]),
    player_vtable: PLAYER_VTABLE,
    step_offset: 0x280,
    attached: 0x2cc,
};
