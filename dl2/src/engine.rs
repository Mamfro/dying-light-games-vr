//! Every fact about this Dying Light 2 build (Steam 1.29.3.0): module fingerprints, function
//! addresses with their exact first instructions, globals and structure offsets. Addresses are
//! from farmerarmor/DyingLight2VR (MIT) and the C++ producer's own research; prologues were
//! decoded from the shipped DLLs.

use monaka_hook::Instruction;

pub use eng_chr::{ENGINE, GAMEDLL};
pub const ENGINE_SHA256: &str = "A06519EE1FD79DE1D665215F38E6B89F2B074BF9745912C64A6F0684617ADB63";
pub const GAMEDLL_SHA256: &str = "B417213B3B1E70B9BB589B88D388BB99DD213B9082D44EF38F4021146F6EE6C4";
pub const RENDERER_11: &str = eng_chr::RD3D11;
pub const RENDERER_11_SHA256: &str = "AF61BE54F24AFDCCDEB396944BADC915EB446B7760AA73B6F2C0D3EF039AC655";
pub const RENDERER_12: &str = eng_chr::RD3D12;
pub const RENDERER_12_SHA256: &str = "5DF9686D1DDD50ED4559CE449AE5A01D2BD03A8A4B9C2D71D0EF9CF3629B61E2";

/// A hook target: its RVA and its first instructions (at least five bytes).
pub type Target = (usize, &'static [Instruction]);

// --- Game DLL: the HUD's world-to-screen projector (farmerarmor/DyingLight2VR, MIT) ------------------
/// `float* project(float* out, const float* world, Camera*, bool clamp, bool* clipped, bool)`.
pub const HUD_PROJECT: Target = (0x1809c50, &[
    Instruction::plain(&[0x48, 0x89, 0x6c, 0x24, 0x10]),
    Instruction::plain(&[0x48, 0x89, 0x74, 0x24, 0x20]),
    Instruction::plain(&[0x57]),
    Instruction::plain(&[0x48, 0x81, 0xec, 0xf0, 0x00, 0x00, 0x00]),
]);
/// Return addresses of the HUD's calls to [`HUD_PROJECT`] (markers and their edge clamp; not the
/// aiming or interaction rays), each checked to call it.
pub const HUD_PROJECT_CALLERS: [usize; 15] =
    [0x165da13, 0x1691fb4, 0x1692036, 0x1693b38, 0x1697b47, 0x169bb5d, 0x169dcd6, 0x169e3f4, 0x16a0421, 0x16a0cc4, 0x16a5b96, 0x16b30c3, 0x16b33d2, 0x16b3462, 0x180a05a];

// --- Engine globals and offsets -----------------------------------------------------------------------
pub const GAME_GLOBAL: usize = 0x272f3f8;
pub const GAME_COUNTER: usize = 0x110;
pub const CAMERA_VTABLE: usize = 0x18a2c68;
pub const THREAD_CONTEXT_GLOBAL: usize = 0x180c6f0;
pub const DRIVER_GLOBAL: usize = 0x28c3360;
/// Return address of the engine's normal present request (the game's own frame).
pub const NORMAL_PRESENT_CALLER: usize = 0x82e200;
/// Return address of the submit call inside the scene.
pub const SUBMIT_FROM_SCENE: usize = 0x8359c7;
/// The level's camera sits this far into the level object.
pub const LEVEL_CAMERA: usize = 0x13b0;
/// Camera state: camera-to-world 3x4 at +0x40, projection 4x4 at +0x80, frustum at +0x1a0.
pub const CAMERA_INVERSE: usize = 0x40;
pub const CAMERA_PROJECTION: usize = 0x80;
pub const CAMERA_FRUSTUM: usize = 0x1a0;
/// The prepared camera pointer in the prepare/submit data.
pub const DATA_CAMERA: usize = 0xd8;
/// The shadow-map jump command's vtable must name the execute we hook.
pub const JUMP_TO_HSM_VTABLE: usize = 0x1a3bfb0;
/// Visibility call sites redirected to the tracked eye's camera (return addresses).
pub const VISIBILITY_CAMERA_CALLER: usize = 0xb493e5;
pub const BASE_CAMERA_COPY_CALLER: usize = 0xb39ad8;
pub const MAIN_VISIBILITY_CALLER: usize = 0xb3a0e7;
pub const WORLD_VISIBILITY_CALLER: usize = 0xb39ec5;
/// `IBaseCamera::FromForwardUpPos`, and its RVA (checked).
pub const FROM_FORWARD_EXPORT: &str = "?FromForwardUpPos@IBaseCamera@@UEAAXAEBVvec3@@00@Z";

// Engine functions the stereo pair calls directly (with their first bytes, checked before use).
pub const COMPONENT_SET: (usize, &[u8]) = (0x111fd20, &[0x40, 0x53, 0x48, 0x83, 0xec, 0x20, 0x48, 0x8b, 0xc2, 0x48, 0x8b, 0xd9]);
pub const REBUILD_PROJECTION: (usize, &[u8]) =
    (0x111ebf0, &[0x4c, 0x8b, 0xdc, 0x49, 0x89, 0x4b, 0x08, 0x53, 0x56, 0x57, 0x48, 0x81, 0xec, 0xc0, 0, 0, 0]);
pub const RESET_LEVEL: (usize, &[u8]) = (0xb43350, &[0x48, 0x89, 0x5c, 0x24, 0x08, 0x57, 0x48, 0x83, 0xec, 0x20, 0x48, 0x8b, 0xf9, 0x48]);
pub const RENDERER_ENTER: (usize, &[u8]) = (0x1110200, &[0x40, 0x53, 0x48, 0x83, 0xec, 0x20, 0x48, 0x8b, 0xd9, 0xf0, 0xff, 0x41, 0x78, 0x80]);
pub const RENDERER_LEAVE: (usize, &[u8]) = (0x1115b20, &[0x40, 0x53, 0x48, 0x83, 0xec, 0x20, 0x80, 0xb9, 0xb8, 0, 0, 0, 0, 0x48]);
/// The scene's submit call site (virtual call +0x1a0), checked so `SUBMIT_FROM_SCENE` is right.
pub const SCENE_SUBMIT_SITE: (usize, &[u8]) = (0x8359b9, &[0x48, 0x8d, 0x54, 0x24, 0x20, 0x48, 0x8b, 0x01, 0xff, 0x90, 0xa0, 0x01, 0x00, 0x00]);

pub type ComponentSetFn = unsafe extern "system" fn(camera: usize, inverse: *const f32, flag: bool);
pub type RebuildFn = unsafe extern "system" fn(camera: usize, flag: bool);
pub type LevelFn = unsafe extern "system" fn(level: usize);
pub type EnterFn = unsafe extern "system" fn(renderer: usize, name: *const u8);
pub type LeaveFn = unsafe extern "system" fn(renderer: usize);

// --- Hooked engine functions --------------------------------------------------------------------------
pub type SceneFn = unsafe extern "system" fn(game: usize, token: u32, a: usize, b: usize) -> usize;
pub const SCENE: Target = (0x835710, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x18])]);
pub type PrepareFn = unsafe extern "system" fn(game: usize, data: usize) -> usize;
pub const PREPARE: Target = (0x82c960, &[
    Instruction::plain(&[0x40, 0x53]),
    Instruction::plain(&[0x55]),
    Instruction::plain(&[0x56]),
    Instruction::plain(&[0x57]),
]);
pub type SubmitFn = unsafe extern "system" fn(renderer: usize, data: usize) -> usize;
pub const SUBMIT: Target = (0x1115ef0, &[Instruction::plain(&[0x40, 0x53]), Instruction::plain(&[0x48, 0x83, 0xec, 0x20])]);
pub type PresentRequestFn = unsafe extern "system" fn(renderer: usize, a: usize, b: usize) -> usize;
pub const PRESENT_REQUEST: Target = (0x11157d0, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]);
pub type ViewSetupFn = unsafe extern "system" fn(level: usize) -> usize;
pub const VIEW_SETUP: Target = (0xb39850, &[Instruction::plain(&[0x40, 0x55]), Instruction::plain(&[0x53]), Instruction::plain(&[0x41, 0x56])]);
pub type VisibilityCameraFn = unsafe extern "system" fn(wrapper: usize) -> usize;
pub const VISIBILITY_CAMERA: Target = (0x61e7c0, &[Instruction::plain(&[0x40, 0x53]), Instruction::plain(&[0x48, 0x83, 0xec, 0x20])]);
pub type BaseCameraCopyFn = unsafe extern "system" fn(output: usize, source: usize) -> usize;
pub const BASE_CAMERA_COPY: Target =
    (0x403d40, &[Instruction::plain(&[0x0f, 0x10, 0x42, 0x10]), Instruction::plain(&[0x0f, 0x11, 0x41, 0x10])]);
pub type MainVisibilityFn = unsafe extern "system" fn(manager: usize, view: u32, camera: usize, input: usize) -> usize;
pub const MAIN_VISIBILITY: Target = (0x6bc2e0, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]);
pub type WorldVisibilityFn = unsafe extern "system" fn(world: usize, camera: usize, input: usize) -> usize;
pub const WORLD_VISIBILITY: Target = (0x70d780, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x10])]);
pub type CommandFn = unsafe extern "system" fn(command: usize, context: usize) -> usize;
pub const JUMP_TO_HSM: Target = (0x116fa10, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]);
pub type HistoryLookupFn = unsafe extern "system" fn(tree: usize, key: *const u32) -> usize;
pub const HISTORY_LOOKUP: Target = (0x110fa90, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x10])]);
pub type ExternalPassFn = unsafe extern "system" fn(object: usize, context: usize) -> usize;
pub const EXTERNAL_PASS: Target =
    (0x116e310, &[Instruction::plain(&[0x48, 0x8b, 0xc4]), Instruction::plain(&[0x55]), Instruction::plain(&[0x56])]);
pub type QueuePhaseFn = unsafe extern "system" fn(phase: u32, data: usize) -> usize;
pub const QUEUE_PHASE: Target = (0x118f910, &[Instruction::plain(&[0x44, 0x8b, 0xc9]), Instruction::plain(&[0x4c, 0x8b, 0xc2])]);
pub type FromForwardFn = unsafe extern "system" fn(camera: usize, forward: *const f32, up: *const f32, position: *const f32);
/// The export loads its state and jumps on: the jump comes along (relocated) into the trampoline.
pub const FROM_FORWARD_UP_POS: Target =
    (0x41daa0, &[Instruction::plain(&[0x48, 0x8b, 0x49, 0x38]), Instruction::rip_relative(&[0xe9, 0x87, 0x21, 0xd0, 0x00], 1)]);

/// Eye-independent render-script commands the right eye skips (vtable RVA, execute RVA, name).
pub const ONCE_COMMANDS: [(usize, usize, &str); 14] = [
    (0x1a3c010, 0x116fab0, "JumpToSpot"),
    (0x1a3adb0, 0x116dbd0, "Envprobes_Update"),
    (0x1a3af90, 0x116dbd0, "GpuFx_Update"),
    (0x1a3b050, 0x116dbd0, "GpuFx_Sort_Bitonic"),
    (0x1a3b0b0, 0x116dbd0, "GpuFx_Sort_Parallel"),
    (0x1a3ae70, 0x11706e0, "WindEmitters_Update"),
    (0x1a3aed0, 0x1170670, "WindCompute_Update"),
    (0x1a3b710, 0x116f4f0, "DispatchWindCompute"),
    (0x1a3be30, 0x11705e0, "UpdateVideos"),
    (0x1a3c190, 0x1170500, "UpdateHeightmap"),
    (0x1a3c310, 0x116e1b0, "DXRTUpdateVertexTransformBLASes"),
    (0x1a3c370, 0x116e1b0, "DXRTUpdateSkinnedBLASes"),
    (0x1a3c3d0, 0x116e1b0, "DXRTBuildBLASes"),
    (0x1a3c430, 0x116e200, "DXRTBuildTLAS"),
];
pub type OnceFn = unsafe extern "system" fn(command: usize, b: usize, c: usize, d: usize) -> usize;
/// The distinct execute functions of `ONCE_COMMANDS`, hooked once each.
pub const ONCE_EXECUTES: [Target; 9] = [
    (0x116fab0, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]),
    (0x116dbd0, &[Instruction::plain(&[0x48, 0x8b, 0x82, 0xc0, 0x22, 0x00, 0x00])]),
    (0x11706e0, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]),
    (0x1170670, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]),
    (0x116f4f0, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]),
    (0x11705e0, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x20])]),
    (0x1170500, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]),
    (0x116e1b0, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]),
    (0x116e200, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x08])]),
];

/// Return addresses (RVAs) of the seven calls through which the scene writes its current camera
/// into the history tree; every other lookup reads the previous camera. Each is checked to be a
/// call of `HISTORY_LOOKUP`.
pub const CAMERA_WRITER_RETURNS: [usize; 7] = [0x11161b6, 0x11161e4, 0x1116243, 0x11162ad, 0x111632d, 0x11163a3, 0x111640d];

// --- Game DLL ---------------------------------------------------------------------------------------
pub type InputActionFn = unsafe extern "system" fn(binding: usize, receivers: usize, value: f32, source: bool, repeat: bool);
pub const INPUT_ACTION: Target = (0x1ec1700, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x10])]);
/// The vertical look actions, checked against the engine's action names: (instruction RVA in the
/// engine that loads the id, name RVA, id, name).
pub const PITCH_ACTIONS: [(usize, usize, u32, &str); 4] = [
    (0xfd3f85, 0x1a06a58, 0x5e, "_ACTION_LOOK_UP"),
    (0xfd3fcc, 0x1a06a68, 0x5f, "_ACTION_LOOK_DOWN"),
    (0xfd4b9c, 0x1a06e48, 0x89, "_ACTION_ROTATE_UP"),
    (0xfd4be5, 0x1a06e60, 0x8a, "_ACTION_ROTATE_DOWN"),
];
/// The player camera update's call of `FromForwardUpPos` (return address).
pub const PLAYER_CAMERA_UPDATE: usize = 0x1194cf3;
/// The player camera's target: the arms visual's `ICameraTarget` base.
pub const CAMERA_TARGET: usize = 0x40;
pub const FPP_VIS_VTABLE: usize = 0x241cdd8;
pub const PLAYER_VTABLE: usize = 0x241a568;
/// The character from the camera target.
pub const TARGET_CHARACTER: usize = 0x570;
/// The player character's look angles in degrees (yaw, pitch negative down) and their targets
/// (head aim writes the targets; the angles are here for the record).
#[allow(dead_code)]
pub const LOOK_YAW: usize = 0xb90;
#[allow(dead_code)]
pub const LOOK_PITCH: usize = 0xb94;
pub const TARGET_YAW: usize = 0xb98;
pub const TARGET_PITCH: usize = 0xb9c;
// The game's yaw turns right as it grows (headset test): `monaka_core::aim::Convention::DEGREES_YAW_RIGHT`.

// --- First-person arms (gamedll; static, 2026-10-06) ---------------------------------------------------
// The arms visual is `PlayerFppVis_PH`; the player camera's +0x40 points at its `ICameraTarget` base
// (+0x5b0, vtable `FPP_VIS_VTABLE`). The player camera update (gamedll 0x1194b90) sets the camera
// (`FromForwardUpPos`), then calls the target's slot 12 with the camera: `FPP_CAMERA_TARGET`, DL1's
// arms callback. Unlike DL1 it changes the arms' bones only with the script switch "legacy anti-wall"
// on (off by default, set by no shipped script); it is the moment the arms are placed for the frame.
// The arms skeleton is a `CoSkeleton` component: its handle at vis+0x90, resolved through the
// engine's component pool. The two weapon visuals sit at vis+0x888 and +0x890.
pub const FPP_CAMERA_TARGET: eng_chr::fpp::Callback = eng_chr::fpp::Callback {
    rva: 0xe20c10,
    prologue: &[Instruction::plain(&[0x48, 0x8b, 0xc4]), Instruction::plain(&[0x48, 0x89, 0x70, 0x18])],
    vtable: FPP_VIS_VTABLE,
    slot: 0x60,
};
/// The `ICameraTarget` base within the vis.
pub const FPP_ICAMERA_TARGET: usize = 0x5b0;
/// The arms skeleton (`eng_chr::coskeleton`): its handle in the vis, and `CoSkeleton`'s vtable
/// slots for world, count, name and set-world.
pub const SKELETON_LAYOUT: eng_chr::coskeleton::Layout =
    eng_chr::coskeleton::Layout { handle: 0x90, world_slot: 0x410, count_slot: 0x488, name_slot: 0x4a8, set_world_slot: 0x428 };
/// The vis's two weapon visuals.
pub const ARMS_LAYOUT: eng_chr::fpp::Layout = eng_chr::fpp::Layout { weapons: 0x888 };
/// DL2's bones against DL1's (the rig's calibrations are DL1's). The right wrist: half turned about
/// its own x (along the forearm), measured 2026-10-06 from the idle melee hold, DL2's `r_hand`
/// against the camera [0.44 0.26 -0.87 | -0.40 -0.81 -0.44 | -0.81 0.54 -0.25] (axes scaled 0.88)
/// against DL1's [-0.11 -0.11 0.99 | -0.24 0.97 0.08 | -0.96 -0.23 -0.14]: x 0.83, y -0.93, z -0.86
/// (the holds differ by some 30 degrees). Without it the hands were upside down. The gun holder: not
/// yet measured.
pub const BONE_AXES: monaka_arms::BoneAxes = monaka_arms::BoneAxes::DYING_LIGHT_2;
/// The finger bones' axes ([`eng_chr::fingers`]; the skeleton probed 2026-10-07: `r_hand` carries
/// the thumb `finger01`-`03` and a metacarpal `finger10`..`40` with three knuckles each).
pub const FINGER_AXES: eng_chr::fingers::Axes = eng_chr::fingers::Axes::DYING_LIGHT_2;
// --- D3D11 renderer -----------------------------------------------------------------------------------
pub type PacketFn = unsafe extern "system" fn(packet: usize) -> usize;
pub const DLSS_EVALUATE_11: Target = (0x37300, &[Instruction::plain(&[0x48, 0x89, 0x5c, 0x24, 0x18])]);
pub const DLSS_CONSTANTS_11: Target =
    (0x57b20, &[Instruction::plain(&[0x48, 0x8b, 0xc4]), Instruction::plain(&[0x55]), Instruction::plain(&[0x53])]);
pub type CommandsFn = unsafe extern "system" fn(object: usize) -> usize;
pub const COMMANDS_11: Target = (0x428d0, &[Instruction::plain(&[0x4c, 0x8b, 0xdc]), Instruction::plain(&[0x49, 0x89, 0x4b, 0x08])]);
/// The queue drain (dispatches queued GPU work) and the driver vtable slot that must name it.
pub const DRAIN_11: (usize, &[u8]) = (0x552e0, &[0x40, 0x53, 0x57, 0x48, 0x83, 0xec, 0x38, 0x32, 0xdb, 0x85, 0xd2, 0x0f, 0x84, 0xe8, 0x02, 0x00]);
pub const DRIVER_VTABLE_11: usize = 0x8ea88;
pub const DRIVER_DRAIN_SLOT_11: usize = 0x5b0;
pub const QUEUE_BANK_11: usize = 0xe9e080;
pub const DLSS_VIEWPORT_11: usize = 0xea5218;

// --- D3D12 renderer -----------------------------------------------------------------------------------
pub use eng_chr::rd3d12::PresentRequestFn as PresentRequest12Fn;
pub const PRESENT_REQUEST_12: Target =
    (0x6ab40, &[Instruction::plain(&[0x48, 0x8b, 0xc4]), Instruction::plain(&[0x55]), Instruction::plain(&[0x53])]);
pub type Packet12Fn = unsafe extern "system" fn(state: usize, packet: usize) -> usize;
pub const DLSS_EVALUATE_12: Target = (0x3a560, &[Instruction::plain(&[0x48, 0x8b, 0xc4]), Instruction::plain(&[0x48, 0x89, 0x58, 0x08])]);
pub const DLSS_CONSTANTS_12: Target = (0x77f20, &[Instruction::plain(&[0x48, 0x8b, 0xc4]), Instruction::plain(&[0x48, 0x89, 0x58, 0x08])]);
/// The present request object (swapchain at +0x60, a non-zero flag at +0x50 when it presents) and
/// the game's direct queue (a Streamline proxy), through a holder pointer.
pub const RD3D12: eng_chr::rd3d12::Layout = eng_chr::rd3d12::Layout { request_active: 0x50, request_swapchain: 0x60, queue_holder: 0x164b50 };
/// DLSS viewport id: (*rd3d12+0x1648a0)+0x50, or the cached copy at +0x30 of the constants state or of
/// the global DLSS state (*rd3d12+0x164ba0).
pub const DLSS_OWNER_12: usize = 0x1648a0;
pub const DLSS_GLOBAL_12: usize = 0x164ba0;

// --- The engine's gui tree (the dynamic HUD; `eng_chr::gui`) ---------------------------------------------
/// The implementations the exported `gui::IElement` getters jump to (RVAs from the jumps, 2026-10-07):
/// `GetWorldMatrix`'s `mov [rsp+0x10], rsi; push rdi; sub rsp, 0x50`; `GetActualPos`'s and
/// `GetActualSize`'s `push rbx; sub rsp, 0x20; mov rbx, rcx`.
pub const GUI_BUILD: eng_chr::gui::Build = eng_chr::gui::Build {
    world: (0xa1cd50, &[Instruction::plain(&[0x48, 0x89, 0x74, 0x24, 0x10]), Instruction::plain(&[0x57]), Instruction::plain(&[0x48, 0x83, 0xec, 0x50])]),
    position: (0xa12dc0, &[Instruction::plain(&[0x40, 0x53]), Instruction::plain(&[0x48, 0x83, 0xec, 0x20]), Instruction::plain(&[0x48, 0x8b, 0xd9])]),
    size: (0xa12de0, &[Instruction::plain(&[0x40, 0x53]), Instruction::plain(&[0x48, 0x83, 0xec, 0x20]), Instruction::plain(&[0x48, 0x8b, 0xd9])]),
};
/// The pieces of the HUD the dynamic HUD takes (widgets of the HUD's documents, probed 2026-10-07):
/// the compass bar on the minimap's panel, the objectives on the quests', the stamina bar on the
/// quick item's. The weapon selector and the objectives were mapped in a safe zone: to be confirmed.
pub const PIECES: [eng_chr::gui::Piece; 5] = {
    use eng_chr::gui::{Extent, Piece};
    [
        Piece { name: "minimap", document: "hud_screen_layer_radar", path: &["radar"], extent: Extent::Declared, size: (896, 192) },
        Piece { name: "weapon", document: "hud_screen_layer_0", path: &["extendend_hud_group", "weapon_selector"], extent: Extent::Declared, size: (768, 384) },
        Piece { name: "quests", document: "hud_screen_layer_0", path: &["extendend_hud_group", "group1", "group1", "wrap_panel", "hud_objectives"], extent: Extent::Leaves, size: (1280, 352) },
        Piece { name: "health", document: "hud_screen_layer_0", path: &["extendend_hud_group", "health_buff_revive_groupped"], extent: Extent::Leaves, size: (1152, 160) },
        Piece { name: "tool", document: "hud_screen_layer_0", path: &["extendend_hud_group", "stamina_bar"], extent: Extent::Leaves, size: (768, 96) },
    ]
};

// --- Physical melee and the movement step (gamedll, engine; 2026-10-08) -------------------------------
/// The melee code (`eng_chr::melee`): the attack start's
/// window at +0x1c8 opens about 180 ms in (measured), its copy at +0x460; attack types 0..=17.
pub const MELEE: eng_chr::melee::Build = eng_chr::melee::Build {
    attack_start: 0x154dd40,
    direction_getter: 0x1596db0,
    test_segments: 0xd3a1b0,
    window_start: 0x1c8,
    window_start_copy: 0x460,
    attack_types: 0x3f3ff,
    segment: 0x34,
    player_vtable: PLAYER_VTABLE,
};
/// The player's movement step (`eng_chr::walk`): the
/// module step is `PlayerBulletPhysicsModule`'s vtable slot 115, `GameStep` the character
/// interface's slot 123. The engine is hash-pinned.
pub const WALK: eng_chr::walk::Build = eng_chr::walk::Build {
    module_step: 0xdd8260,
    game_step: 0xe700d0,
    game_step_prologue: None,
    player_vtable: PLAYER_VTABLE,
    step_offset: 0x1e0,
    attached: 0x22c,
};

// --- Video settings (headset resolution) ---------------------------------------------------------------
/// CVideoSettings: settings struct at +8 (+4 window mode, +8/+0xc resolution, +0xb8 change mask);
/// +0x699/+0x69b request an apply on the next frame.
pub const VIDEO_SETTINGS_IN_GAME: usize = 0xe0;
