//! Dying Light: The Beast (D3D12) in head-tracked stereo, injected by `monaka_loader`. Options come
//! from `beast-options.txt` beside the DLL copy; the research probes' are in `research`.
//!
//! The adapter ([`Beast::ADAPTER`]), in the order its components start, by feature: [`stereo`]
//! (the camera setters, the present request, the present loop, the eye size, the eyes), [`look`]
//! (head aim, the level view), [`arms`] (the hand rig, fingers, melee, handwork, room-scale),
//! [`GRABS`], the [`flashlight`], the [`pad`], [`dlss`]'s inputs, the [`hud`], the probes and the
//! run's [`report`].

pub mod engine;
mod manifest;
#[cfg(test)]
mod launch_tests;
mod arms;
mod dlss;
mod flashlight;
mod hud;
mod look;
mod pad;
mod report;
mod research;
mod stereo;

use eng_chr::feature::{Chrome, Facts as ChromeFacts, Look};
use eng_chr::headaim::IdLoad;
use monaka_core::manifest::Manifest;
use monaka_core::options::Options;
use monaka_hook::module::Module;
use monaka_producer::game::{Adapter, Component, Game};
use monaka_producer::{Rejection, Session, log, require_build};
use windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue;

monaka_producer::producer!(game: crate::Beast);

/// Dying Light: The Beast, as Monaka runs it.
pub struct Beast;

/// This build's modules: the renderer (the `CCamera` exports), the engine, the game DLL (with
/// stereo; its player camera update checked) and the D3D12 renderer (when the game runs on it).
pub struct Facts {
    pub renderer: Module,
    pub engine: Module,
    pub gamedll: Option<Module>,
    pub renderer_12: Option<Module>,
}

impl Game for Beast {
    const MANIFESTS: &'static [Manifest] = manifest::MANIFESTS;
    const ADAPTER: Adapter<Beast> = Adapter {
        components: &[
            &stereo::CAMERAS,
            &stereo::PRESENT_REQUEST,
            &stereo::PRESENT,
            &stereo::EYE_SIZE,
            &stereo::EYES,
            &look::LOOK,
            &arms::ARMS,
            &arms::FINGERS,
            &arms::MELEE,
            &arms::HANDWORK,
            &arms::ROOM_SCALE,
            &look::VIEW_BOB,
            &GRABS,
            &flashlight::FLASHLIGHT,
            &pad::GAMEPAD,
            &dlss::STREAMLINE,
            &hud::HUD,
            &hud::PANELS,
            &PROBES,
            &report::REPORT,
        ],
    };
    type Build = Facts;
    type Frame = ();

    fn prepare(_: &'static Session, _: &'static Manifest, options: &Options) -> Result<Facts, Rejection> {
        let renderer = require_build(engine::RENDERER, engine::RENDERER_SHA256)?;
        log!("{} at {:#x}", engine::RENDERER, renderer.base());
        let engine_module = Module::find(engine::ENGINE).ok_or_else(|| Rejection::not_ready("the engine is not loaded"))?;
        let stereo = stereo_on(options);
        let gamedll = if stereo || stereo::turn_yaw(options).is_some() {
            let gamedll = require_build(engine::GAMEDLL, engine::GAMEDLL_SHA256)?;
            let (call, bytes) = engine::PLAYER_CAMERA_CALL;
            if !gamedll.bytes_match(call, bytes) {
                return Err(Rejection::revision("the player camera update's call is not where it was inspected"));
            }
            Some(gamedll)
        } else {
            None
        };
        let renderer_12 = match Module::find(engine::RENDERER_12) {
            Some(_) => Some(require_build(engine::RENDERER_12, engine::RENDERER_12_SHA256)?),
            None if stereo => return Err(Rejection::unsupported("stereo needs the game on DirectX 12")),
            None => {
                log!("{} is not loaded (D3D11?): no present probe", engine::RENDERER_12);
                None
            }
        };
        research::PROBES.configure(options);
        Ok(Facts { renderer, engine: engine_module, gamedll, renderer_12 })
    }
}

impl Chrome for Beast {
    const FACTS: ChromeFacts = ChromeFacts {
        look: Some(Look { aim: &look::AIM, input_action: engine::INPUT_ACTION, pitch_actions: &engine::PITCH_ACTIONS, id_load: IdLoad::EdxImm32 }),
        view_bob: Some(engine::VIEW_BOB),
        grabs: Some(engine::GRABS),
        walk: Some(engine::WALK),
        melee: Some(engine::MELEE),
        melee_reach: Some(engine::MELEE_REACH),
        attacks_on_release: true,
        handwork: Some(engine::HANDWORK),
        gui: Some(engine::GUI_BUILD),
        pieces: &engine::PIECES,
        hud_over_eyes: true,
    };

    fn engine(build: &Facts) -> &Module {
        &build.engine
    }

    fn gamedll(build: &Facts) -> Option<&Module> {
        build.gamedll.as_ref()
    }

    fn game_queue() -> Option<ID3D12CommandQueue> {
        stereo::present12::game_queue()
    }

    fn showing() -> bool {
        stereo::eyes::running()
    }
}

/// Stereo (`stereo=0`: the probes only, the game as it plays on the monitor).
pub(crate) fn stereo_on(options: &Options) -> bool {
    options.switch("stereo", true)
}

/// Zombies' grabs never land on the player (`eng_chr::grabs`).
pub const GRABS: Component<Beast> = eng_chr::feature::grabs();

/// The research probes: their hooks and reports. Each probe checks its own option.
pub const PROBES: Component<Beast> = Component { install: research::install_probes, release: research::report, ..monaka_producer::game::probes() };
