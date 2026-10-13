//! Dying Light 2 (Steam 1.29.3.0) in head-tracked stereo, injected by `monaka_loader`.
//!
//! The game's renderer is whichever rd3d DLL it loaded (video.scr RendererMode):
//! - D3D11 (the game's default): same-frame stereo. The game's own scene call renders the left eye;
//!   the right eye is a second scene call on the same simulation frame. Both eyes are copied from
//!   the back buffer as the engine dispatches its presents and go to the viewer over the keyed-mutex
//!   channel with the head pose they were rendered with.
//! - D3D12: same-frame stereo with frame generation per eye (`mode=framegen`, the default) or
//!   without (`mode=same`), alternate-eye (`mode=alternate`) or depth stereo (`mode=depth`: one
//!   centre render, both eyes from its depth), over the fence channel.
//!
//! Menus, loading and any frame without a fresh head pose are published mono (the viewer then shows
//! a head-locked screen). Engine and renderer addresses are from farmerarmor/DyingLight2VR (MIT),
//! each guarded by its byte signature before use; options come from `dl2-options.txt` beside the
//! DLL copy. The probes are in `research`.
//!
//! The adapter ([`Dl2::ADAPTER`]), in the order its components start, by feature: stereo (the eye
//! size, Monaka's same-frame loop with the pair on the engine side, frame generation, depth to the
//! headset, foveation, the frame inputs), look (head aim at the player camera update, the pitch
//! lock, the level view), the arms (the hand rig, fingers, melee, the bow, throw and lockpick,
//! reloading by hand, room-scale), [`GRABS`], the pad, the HUD (the layer, its pieces on the hands,
//! the world markers) and the probes.
//!
//! The code is sorted by who should own it: [`providers`] (what only Dying Light 2 knows: its
//! facts, its hooks, reading and writing the game), [`extras`] (features only this game has) and
//! [`research`] (the probes); it has no cooking left. The components are listed here, the extras'
//! beside their features.

mod config;
mod extras;
mod manifest;
#[cfg(test)]
mod launch_tests;
mod providers;
mod research;

use config::Config;
use eng_chr::feature::{Chrome, Facts as ChromeFacts, Look};
use eng_chr::providers::look_fields::IdLoad;
use eng_chr::providers::rig::Arms;
use monaka_core::feature::Feature;
use monaka_core::manifest::{CONTROLLER_PAD, CONTROLLER_PROXY, HEAD_MOVEMENT, Manifest, OWN_EYE_SIZE, RENDER_SIZE};
use monaka_core::options::Options;
use monaka_hook::module::Module;
use monaka_body::reload::Reloads;
use monaka_producer::game::{Adapter, Component, Game, Lack, On, Start};
use monaka_producer::{Rejection, Session, log, require_build};
use monaka_stereo::eye_size;
use monaka_stereo::providers::{GameQueue, frame_inputs};
use monaka_stereo::same_frame::PAIRS;
use providers::{engine, eye_site, game, present};
use windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue;

monaka_producer::producer!(game: crate::Dl2);

/// Dying Light 2, as Monaka runs it.
pub struct Dl2;

/// This build's modules: the engine, the renderer it loaded (D3D11 or D3D12) and the game DLL (when
/// it is the inspected build: head aim and the hands need it).
pub struct Facts {
    pub engine: Module,
    pub renderer: Module,
    pub d3d11: bool,
    pub gamedll: Option<Module>,
}

impl Game for Dl2 {
    const MANIFESTS: &'static [Manifest] = manifest::MANIFESTS;
    const ADAPTER: Adapter<Dl2> = Adapter {
        components: &[
            &EYE_SIZE,
            &PRESENT,
            &EYES,
            &FRAMEGEN,
            &PER_EYE_FRAMEGEN,
            &DEPTH,
            &FOVEATION,
            &INPUTS,
            &LOOK,
            &PITCH_LOCK,
            &HAND_RIG,
            &FINGERS,
            &MELEE,
            &BOW,
            &THROW,
            &LOCKPICK,
            &RELOAD_BY_HAND,
            &ROOM_SCALE,
            &VIEW_BOB,
            &GRABS,
            &ACTIONS,
            &GAMEPAD,
            &HUD,
            &PANELS,
            &extras::markers::MARKERS,
            &PROBES,
        ],
    };
    type Build = Facts;
    type Frame = ();

    fn prepare(_: &'static Session, manifest: &'static Manifest, options: &Options) -> Result<Facts, Rejection> {
        let engine_module = require_build(eng_chr::ENGINE, engine::ENGINE_SHA256)?;
        let (renderer11, renderer12) = (Module::find(engine::RENDERER_11), Module::find(engine::RENDERER_12));
        let renderer = match (&renderer11, &renderer12) {
            (Some(_), None) => require_build(engine::RENDERER_11, engine::RENDERER_11_SHA256)?,
            (None, Some(_)) => require_build(engine::RENDERER_12, engine::RENDERER_12_SHA256)?,
            _ => return Err(Rejection::not_ready("expected exactly one of rd3d11/rd3d12 loaded")),
        };
        let d3d11 = renderer11.is_some();
        let mut config = Config::from_options(options, manifest);
        if config::framegen_asked(options) && !config.framegen {
            log!("frame generation runs with Standard (Same-frame) only in this game; off");
        }
        if d3d11 && (config.alternate_eye || config.depth_stereo || config.framegen) {
            log!("alternate-eye, depth stereo and frame generation need DirectX 12; using same-frame stereo");
            (config.alternate_eye, config.depth_stereo, config.framegen) = (false, false, false);
        }
        config::set(config);
        log!("{config:?}");
        research::PROBES.configure(options);
        // The game DLL carries head aim and the hands; a different build only loses those.
        let gamedll = Module::find(eng_chr::GAMEDLL).filter(|m| match m.file_sha256() {
            Some(hash) if hash == engine::GAMEDLL_SHA256 => true,
            other => {
                log!("{} is build {other:?}, not the inspected one; head aim and the hands are off", eng_chr::GAMEDLL);
                false
            }
        });
        game::set(game::Game {
            engine: engine_module.base(),
            gamedll: gamedll.as_ref().map(Module::base),
            renderer11: renderer11.as_ref().map(Module::base),
            renderer12: renderer12.as_ref().map(Module::base),
            component_set: engine_function(&engine_module, engine::COMPONENT_SET, "component set")?,
            rebuild_projection: engine_function(&engine_module, engine::REBUILD_PROJECTION, "rebuild projection")?,
            reset_level: engine_function(&engine_module, engine::RESET_LEVEL, "reset level")?,
            renderer_enter: engine_function(&engine_module, engine::RENDERER_ENTER, "renderer enter")?,
            renderer_leave: engine_function(&engine_module, engine::RENDERER_LEAVE, "renderer leave")?,
        });
        if d3d11 && !present::drain_available(renderer.base()) {
            return Err(Rejection::revision("signature mismatch: renderer queue drain"));
        }
        Ok(Facts { engine: engine_module, renderer, d3d11, gamedll })
    }
}

impl Chrome for Dl2 {
    const FACTS: ChromeFacts = ChromeFacts {
        // The engine does not export the cutscene query: head aim steers through cutscenes.
        look: Some(Look { aim: &providers::look_fields::AIM, input_action: engine::INPUT_ACTION, pitch_actions: &engine::PITCH_ACTIONS, id_load: IdLoad::MemImm32, cutscenes: false }),
        arms: Some(Arms { rig: &providers::hand_rig::RIG, moment: providers::hand_rig::moment, head_aim_here: false }),
        view_bob: Some(engine::VIEW_BOB),
        grabs: Some(engine::GRABS),
        walk: Some(engine::WALK),
        melee: Some(engine::MELEE),
        melee_reach: None,
        attacks_on_release: false,
        hands: Some(engine::HANDS),
        reload: Some(engine::RELOAD),
        gui: Some(engine::GUI_BUILD),
        pieces: &engine::PIECES,
        hud_over_eyes: false,
    };

    fn engine(build: &Facts) -> &Module {
        &build.engine
    }

    fn gamedll(build: &Facts) -> Option<&Module> {
        build.gamedll.as_ref()
    }

    fn showing() -> bool {
        PAIRS.publishing()
    }
}

impl GameQueue for Dl2 {
    fn game_queue() -> Option<ID3D12CommandQueue> {
        present::game_queue()
    }
}

/// Reloading by hand reads the palms the hand rig notes (`HandRig::trail`), at each update of the
/// player's ready gun, the hook the reload is taken by (`eng_chr::providers::actions`).
impl Reloads for Dl2 {
    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        eng_chr::feature::hook_reload(start)
    }

    fn report() {
        eng_chr::providers::actions::report_reload();
    }
}

/// An engine function the pair calls, after checking its first bytes.
fn engine_function<F: Copy>(engine: &Module, (rva, bytes): (usize, &[u8]), name: &str) -> Result<F, Rejection> {
    if !engine.bytes_match(rva, bytes) {
        return Err(Rejection::revision(format!("signature mismatch: engine function {name}")));
    }
    let address = engine.at(rva);
    // SAFETY: F is the function's pointer type (engine.rs), and its code is the inspected build's.
    Ok(unsafe { std::mem::transmute_copy(&address) })
}

// ---- Stereo ----

/// The game renders at the headset's eye size, windowed, while VR runs (`monaka_stereo::eye_size`,
/// [`providers::video`]): asked for at the first presents, publishing held until the game presents
/// at it.
pub const EYE_SIZE: Component<Dl2> = Component { options: &[RENDER_SIZE, OWN_EYE_SIZE], ..eye_size::eye_size() };

/// The same-frame loop (`monaka_stereo::same_frame`): the head, each eye's frame taken at the
/// present entry ([`providers::present`]) and the pairs published, the eye site installed
/// ([`providers::eye_site`]).
pub const PRESENT: Component<Dl2> = monaka_stereo::same_frame::same_frame();

/// Stereo by the method the run picked (`stereo`: Standard (Same-frame), AER (Alt-eye) or
/// Depth-stereo (synthetic)), on the same-frame loop: the pair on the engine side, each
/// eye's camera and frustum written into the scene ([`providers::eye_site`]). Always on: menus and
/// loading go out flat.
pub const EYES: Component<Dl2> = Component {
    name: "eyes",
    features: &[Feature::Stereo, Feature::PerEyeHistory],
    options: &[config::STEREO, config::FRAMEGEN, HEAD_MOVEMENT],
    needs: &[&PRESENT],
    settings: |options, manifest| {
        Config::from_options(options, manifest);
    },
    arm: eye_site::started,
    give_back: eye_site::give_back,
    release: eye_site::release,
    summary: Some(monaka_stereo::same_frame::timing_report),
    ..Component::NONE
};

/// FSR frame generation loaded, for Standard (Same-frame) with frame generation
/// ([`PER_EYE_FRAMEGEN`]).
pub const FRAMEGEN: Component<Dl2> = Component { needs: &[&EYES], on: |_| On::when(config::get().framegen, "no frame generation for this method"), ..monaka_framegen::feature::framegen() };

/// Standard (Same-frame) with frame generation: per-eye frame generation of the same-frame pairs,
/// the generated pair between the real ones (`monaka_stereo::framegen12::pairs`).
pub const PER_EYE_FRAMEGEN: Component<Dl2> = Component { needs: &[&FRAMEGEN], ..monaka_stereo::framegen12::pairs::per_eye_pairs() };

/// Each eye's depth to the headset with its pair (`headset_depth`, on by default): the rendered
/// eyes' pairs (Standard and AER) carry it (the same-frame loop's D3D12 pairs).
pub const DEPTH: Component<Dl2> = Component { needs: &[&EYES], ..monaka_stereo::depth_layer::depth_layer() };

/// Foveated rendering on D3D12 (`foveated_rendering`, on by default).
pub const FOVEATION: Component<Dl2> =
    Component { needs: &[&EYES], on: |run| On::when(!run.build.d3d11, "foveated rendering needs D3D12"), ..monaka_stereo::foveate12::foveation() };

/// Each eye's inputs for the features that ask (frame generation, depth to the headset,
/// foveation's render size), from the Streamline calls [`providers::streamline`] hooks (the frame
/// inputs provider: [`providers::stereo`]).
pub const INPUTS: Component<Dl2> = frame_inputs();

// ---- Look ----

/// Head aim on the character (`monaka_body::look`, its targets delivery on DL2's look fields:
/// [`providers::look_fields`]), steered at the player camera update (`FromForwardUpPos`).
pub const LOOK: Component<Dl2> = Component { needs: &[&EYES], ..monaka_body::look::look() };

/// Without head aim, vertical look passes only back toward level (`monaka_body::pitch_lock`, at the
/// input-action converter: [`providers::look_fields`]).
pub const PITCH_LOCK: Component<Dl2> = Component { needs: &[&EYES], ..monaka_body::pitch_lock::pitch_lock() };

/// The view stays level while head aim steers (`eng_chr::providers::view_bob`).
pub const VIEW_BOB: Component<Dl2> = Component { needs: &[&LOOK], ..eng_chr::feature::view_bob() };

// ---- The arms ----

/// The arms on the controllers (`eng_chr::feature::hand_rig`, on DL2's rig: [`providers::hand_rig`]),
/// steered by head aim first.
pub const HAND_RIG: Component<Dl2> = Component { needs: &[&LOOK], ..eng_chr::feature::hand_rig() };

/// A free hand's fingers on the tracked fingers (the hand rig's, at the arms' pose).
pub const FINGERS: Component<Dl2> = Component { needs: &[&HAND_RIG], ..monaka_body::feature::fingers() };
pub const MELEE: Component<Dl2> = Component { needs: &[&HAND_RIG], ..eng_chr::feature::melee() };
/// The bow drawn and loosed by the hands (`monaka_body::bow`, on [`providers::arms`]).
pub const BOW: Component<Dl2> = Component { needs: &[&HAND_RIG], ..eng_chr::feature::bow() };
/// A throwable leaves the left hand as it threw it (`monaka_body::throw`, on [`providers::arms`]).
pub const THROW: Component<Dl2> = Component { needs: &[&HAND_RIG], ..eng_chr::feature::throw() };
/// Twisting the wrists picks a lock (`monaka_body::lockpick`, on [`providers::actions`]).
pub const LOCKPICK: Component<Dl2> = Component { needs: &[&HAND_RIG], ..eng_chr::feature::lockpick() };
/// The left hand to the gun and away reloads (`monaka_body::reload`).
pub const RELOAD_BY_HAND: Component<Dl2> = Component { needs: &[&HAND_RIG], ..monaka_body::reload::reload() };
pub const ROOM_SCALE: Component<Dl2> = Component { needs: &[&HAND_RIG], ..monaka_body::feature::room_scale() };

/// Zombies' grabs never land on the player (`eng_chr::providers::grabs`).
pub const GRABS: Component<Dl2> = eng_chr::feature::grabs();

// ---- The pad ----

/// The actions provider ([`providers::actions`]): ready-mades press the game's actions by its
/// bindings.
pub const ACTIONS: Component<Dl2> = monaka_pad::feature::actions();

/// The controllers as the game's pad, the whole right stick to the game (head aim and the pitch
/// lock decide what its pitch does).
pub const GAMEPAD: Component<Dl2> = Component { options: &[CONTROLLER_PAD, CONTROLLER_PROXY], arm: monaka_pad::feature::whole_stick, ..monaka_pad::feature::gamepad() };

// ---- The HUD ----

/// The HUD layer at DL2's own size, its distance with it (where it goes over each eye: `HudLayer
/// for Dl2` in [`providers::stereo`], laid over by [`providers::hud`], and Depth-stereo's centre
/// frame in [`extras::depth_stereo`]).
pub const HUD: Component<Dl2> = Component { options: &monaka_stereo::hud_layer::options(0.55), needs: &[&EYES], ..eng_chr::feature::hud() };
/// The HUD's pieces on the hands (`eng_chr::feature`'s ready-made); its world markers are
/// [`extras::markers`].
pub const PANELS: Component<Dl2> = Component { needs: &[&HUD], ..eng_chr::feature::panels() };


/// The research probes: their hooks and reports. Each probe checks its own option.
pub const PROBES: Component<Dl2> = Component { install: research::install_probes, release: research::report, ..monaka_producer::game::probes() };
