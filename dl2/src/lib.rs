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
//! The adapter ([`Dl2::ADAPTER`]), in the order its components start, by feature: [`stereo`] (the
//! pair on the engine side, the eyes leaving the renderer), [`look`] (head aim at the player camera
//! update, the pitch lock, the level view), [`arms`] (the hand rig, fingers, melee, handwork,
//! room-scale), [`GRABS`], the [`pad`], the [`hud`] (the layer, its pieces on the hands, the world
//! markers) and the probes.

mod arms;
mod config;
mod engine;
mod game;
mod hud;
mod look;
mod manifest;
#[cfg(test)]
mod launch_tests;
mod pad;
mod research;
mod stereo;

use config::Config;
use eng_chr::feature::{Chrome, Facts as ChromeFacts, Look};
use eng_chr::headaim::IdLoad;
use monaka_core::manifest::Manifest;
use monaka_core::options::Options;
use monaka_hook::module::Module;
use monaka_producer::game::{Adapter, Component, Game};
use monaka_producer::{Rejection, Session, log, require_build};
use stereo::{framegen, render11, render12};
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
            &stereo::EYES,
            &look::LOOK,
            &look::PITCH,
            &arms::ARMS,
            &arms::FINGERS,
            &arms::MELEE,
            &arms::HANDWORK,
            &arms::ROOM_SCALE,
            &look::VIEW_BOB,
            &GRABS,
            &pad::GAMEPAD,
            &hud::HUD,
            &hud::PANELS,
            &hud::MARKERS,
            &PROBES,
        ],
    };
    type Build = Facts;
    type Frame = ();

    fn prepare(_: &'static Session, manifest: &'static Manifest, options: &Options) -> Result<Facts, Rejection> {
        let engine_module = require_build(engine::ENGINE, engine::ENGINE_SHA256)?;
        let (renderer11, renderer12) = (Module::find(engine::RENDERER_11), Module::find(engine::RENDERER_12));
        let renderer = match (&renderer11, &renderer12) {
            (Some(_), None) => require_build(engine::RENDERER_11, engine::RENDERER_11_SHA256)?,
            (None, Some(_)) => require_build(engine::RENDERER_12, engine::RENDERER_12_SHA256)?,
            _ => return Err(Rejection::not_ready("expected exactly one of rd3d11/rd3d12 loaded")),
        };
        let d3d11 = renderer11.is_some();
        let mut config = Config::from_options(options, manifest);
        if d3d11 && (config.alternate_eye || config.depth_stereo || config.framegen) {
            log!("alternate-eye, depth stereo and frame generation need DirectX 12; using same-frame stereo");
            (config.alternate_eye, config.depth_stereo, config.framegen) = (false, false, false);
        }
        if config.framegen {
            match monaka_framegen::load() {
                Some(api) => {
                    framegen::enable(api);
                    log!("FSR frame generation per eye: on");
                }
                None => {
                    log!("FSR frame generation unavailable ({} not loadable); same-frame stereo without it", monaka_framegen::ffx::LOADER);
                    config.framegen = false;
                }
            }
        }
        config::set(config);
        log!("{config:?}");
        research::PROBES.configure(options);
        // The game DLL carries head aim and the hands; a different build only loses those.
        let gamedll = Module::find(engine::GAMEDLL).filter(|m| match m.file_sha256() {
            Some(hash) if hash == engine::GAMEDLL_SHA256 => true,
            other => {
                log!("{} is build {other:?}, not the inspected one; head aim and the hands are off", engine::GAMEDLL);
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
        if d3d11 && !render11::drain_available(renderer.base()) {
            return Err(Rejection::revision("signature mismatch: renderer queue drain"));
        }
        Ok(Facts { engine: engine_module, renderer, d3d11, gamedll })
    }
}

impl Chrome for Dl2 {
    const FACTS: ChromeFacts = ChromeFacts {
        look: Some(Look { aim: &look::AIM, input_action: engine::INPUT_ACTION, pitch_actions: &engine::PITCH_ACTIONS, id_load: IdLoad::MemImm32 }),
        view_bob: Some(engine::VIEW_BOB),
        grabs: Some(engine::GRABS),
        walk: Some(engine::WALK),
        melee: Some(engine::MELEE),
        melee_reach: None,
        attacks_on_release: false,
        handwork: Some(engine::HANDWORK),
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

    fn game_queue() -> Option<ID3D12CommandQueue> {
        render12::game_queue()
    }

    fn showing() -> bool {
        stereo::scene::publishing()
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

/// Zombies' grabs never land on the player (`eng_chr::grabs`).
pub const GRABS: Component<Dl2> = eng_chr::feature::grabs();

/// The research probes: their hooks and reports. Each probe checks its own option.
pub const PROBES: Component<Dl2> = Component { install: research::install_probes, release: research::report, ..monaka_producer::game::probes() };
