//! Dying Light: The Beast (D3D12) in head-tracked stereo, injected by `monaka_loader`. Options come
//! from `beast-options.txt` beside the DLL copy; the research probes' are in `research`.
//!
//! The adapter ([`Beast::ADAPTER`]), in the order its components start, by feature: stereo (the
//! camera setters, the present request, the present loop, the eye size, the eyes, frame
//! generation, frame warping, depth to the headset, the frame inputs), look (head aim, the pitch
//! lock, the level view), the arms (the hand rig, fingers, melee, the bow and throw, reloading by hand, room-scale),
//! [`GRABS`], the flashlight, the pad, the upscaler per eye and DLSS's inputs, FSR's inputs,
//! [`FOVEATION`], the HUD and the probes.
//!
//! The code is sorted by who should own it: [`providers`] (what only The Beast knows: its facts,
//! its hooks, reading and writing the game) and [`extras`] (features only this game has); its
//! cooking is the Chrome Engine's, in `eng_chr::cooking`, and [`research`] holds the probes. The
//! components are listed here, the extras' beside their features.

mod config;
mod extras;
mod manifest;
#[cfg(test)]
mod launch_tests;
pub mod providers;
mod research;

use config::framegen_on;
use eng_chr::feature::{Chrome, Facts as ChromeFacts, Look};
use eng_chr::providers::look_fields::IdLoad;
use eng_chr::providers::rig::Arms;
use monaka_core::manifest::{
    CONTROLLER_PAD, CONTROLLER_PROXY, EYE_SEPARATION, HEAD_MOVEMENT, Kind, LATENCY, Manifest, OWN_EYE_SIZE, Opt, RENDER_SIZE, Shown, dpad_hold, line,
};
use monaka_core::options::Options;
use monaka_core::rendering::StereoMethod;
use monaka_hook::module::Module;
use monaka_body::reload::Reloads;
use monaka_producer::game::{Adapter, Component, Every, Game, Lack, On, Start, Switch};
use monaka_producer::{Rejection, Session, log, require_build};
use monaka_stereo::providers::{GameQueue, frame_inputs};
use monaka_stereo::upscaler::UPSCALE_PER_EYE;
use monaka_stereo::{afw12, depth_layer, foveate12, framegen12};
use providers::{cameras, dlss, engine, eye_site, fsr, present12, video};
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
            &CAMERAS,
            &PRESENT_REQUEST,
            &PRESENT,
            &EYE_SIZE,
            &EYES,
            &FRAMEGEN,
            &PER_EYE_FRAMEGEN,
            &AFW,
            &AFW_FRAMEGEN,
            &DEPTH,
            &INPUTS,
            &LOOK,
            &PITCH_LOCK,
            &HAND_RIG,
            &FINGERS,
            &MELEE,
            &BOW,
            &THROW,
            &RELOAD_BY_HAND,
            &ROOM_SCALE,
            &VIEW_BOB,
            &GRABS,
            &extras::flashlight::FLASHLIGHT,
            &ACTIONS,
            &GAMEPAD,
            &PER_EYE,
            &STREAMLINE,
            &FSR,
            &FOVEATION,
            &HUD,
            &PANELS,
            &PROBES,
        ],
    };
    type Build = Facts;
    type Frame = ();

    fn prepare(_: &'static Session, _: &'static Manifest, options: &Options) -> Result<Facts, Rejection> {
        let renderer = require_build(engine::RENDERER, engine::RENDERER_SHA256)?;
        log!("{} at {:#x}", engine::RENDERER, renderer.base());
        let engine_module = Module::find(eng_chr::ENGINE).ok_or_else(|| Rejection::not_ready("the engine is not loaded"))?;
        let stereo = stereo_on(options);
        let gamedll = if stereo || cameras::turn_yaw(options).is_some() {
            let gamedll = require_build(eng_chr::GAMEDLL, engine::GAMEDLL_SHA256)?;
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
        // The engine does not export the cutscene query: head aim steers through cutscenes.
        look: Some(Look { aim: &providers::look_fields::AIM, input_action: engine::INPUT_ACTION, pitch_actions: &engine::PITCH_ACTIONS, id_load: IdLoad::EdxImm32, cutscenes: false }),
        arms: Some(Arms { rig: &providers::hand_rig::RIG, moment: providers::hand_rig::moment, head_aim_here: true }),
        view_bob: Some(engine::VIEW_BOB),
        grabs: Some(engine::GRABS),
        walk: Some(engine::WALK),
        melee: Some(engine::MELEE),
        melee_reach: Some(engine::MELEE_REACH),
        attacks_on_release: true,
        hands: Some(engine::HANDS),
        reload: Some(engine::RELOAD),
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

    fn showing() -> bool {
        eye_site::running()
    }
}

impl GameQueue for Beast {
    fn game_queue() -> Option<ID3D12CommandQueue> {
        present12::game_queue()
    }
}

/// Reloading by hand reads the palms the hand rig notes (`HandRig::trail`), at each update of the
/// player's ready gun, the hook the reload is taken by (`eng_chr::providers::actions`).
impl Reloads for Beast {
    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        eng_chr::feature::hook_reload(start)
    }

    fn report() {
        eng_chr::providers::actions::report_reload();
    }
}

/// Stereo (`stereo=0`: the probes only, the game as it plays on the monitor).
pub(crate) fn stereo_on(options: &Options) -> bool {
    options.switch("stereo", true)
}

// ---- Stereo ----

/// The renderer camera's exported setters ([`cameras`]): the player camera update passes
/// `SetView(vec3)`, where the eye camera, head aim and the turn check take over, and
/// `ComputeFrustumMatrix`, where the headset's field of view goes in. Always on: the probes ride on
/// them too.
pub const CAMERAS: Component<Beast> = Component { name: "cameras", install: cameras::install_cameras, quiet: providers::camera_update::disable, ..Component::NONE };

/// The D3D12 present request ([`present12`]): the HUD laid over each frame, the present made inside
/// the present loop.
pub const PRESENT_REQUEST: Component<Beast> = Component {
    name: "present request",
    install: present12::install_present,
    every: &[Every::millis(2000, present12::report)],
    ..Component::NONE
};

/// Monaka's present loop, entered from the present request; it starts with the eyes, once the eye
/// size is asked for ([`eye_site`]).
pub const PRESENT: Component<Beast> = Component {
    needs: &[&PRESENT_REQUEST],
    switch: Some(Switch::new("stereo", true, "stereo is off: the probes only")),
    arm: || {},
    ..monaka_stereo::feature::present()
};

const RENDER_SIZE_NOTED: Opt = RENDER_SIZE.eye_size_note("Click into the game: it switches to {size} once it has focus, and VR shows it from then on.");

/// The game renders at the headset's eye size, windowed, while VR runs (`monaka_stereo::eye_size`,
/// [`video`]): asked for at the presents once the hooks are in, publishing held until the game
/// presents at it, so it starts before the eyes.
pub const EYE_SIZE: Component<Beast> =
    Component { options: &[RENDER_SIZE_NOTED, OWN_EYE_SIZE], needs: &[&PRESENT], install: video::install_eye_size, ..monaka_stereo::eye_size::eye_size() };

/// Stereo by alternate eyes ([`eye_site`]): after each present the next eye and the head pose are
/// chosen, the player camera update sets the camera to that eye, and the frame is labelled by its
/// camera as DLSS gets it; `latency` presents later it goes out with its partner eye. The stereo
/// method is offered here (`stereo=aer|afw`); `stereo=0` is the present loop's switch (the probes
/// only, [`PRESENT`]). `eye_test=` and `latency_test=` (degrees) turn the eyes or every other
/// pair's head for PC checks.
pub const EYES: Component<Beast> = Component {
    options: &[config::STEREO, HEAD_MOVEMENT, LATENCY, EYE_SEPARATION],
    switch: None,
    settings: |options, _| {
        config::method(options);
    },
    needs: &[&PRESENT],
    needs_channel: true,
    arm: eye_site::arm,
    quiet: || eye_site::quiet(1000),
    every: &[Every::millis(2000, eye_site::report_rates)],
    release: eye_site::release,
    ..monaka_stereo::eye_site::eyes()
};

/// FSR frame generation loaded (`framegen=fsr`), for AFW's in-between pairs ([`AFW_FRAMEGEN`]) and
/// AER's per-eye frame generation ([`PER_EYE_FRAMEGEN`]).
pub const FRAMEGEN: Component<Beast> = Component {
    options: &[config::FRAMEGEN],
    needs: &[&EYES],
    settings: |options, _| {
        framegen_on(options);
    },
    on: |run| On::when(framegen_on(run.options), "frame generation is off"),
    ..monaka_framegen::feature::framegen()
};

/// AER with frame generation: each eye's generated frame of the other eye's newest moment, its own
/// pairs.
pub const PER_EYE_FRAMEGEN: Component<Beast> =
    Component { needs: &[&FRAMEGEN], on: |run| On::when(config::method(run.options) == StereoMethod::Aer, "AFW chosen"), ..framegen12::per_eye() };

/// AFW (Frame-warp): each frame warped into the other eye at the same head pose, so every present
/// is a pair of one moment.
pub const AFW: Component<Beast> =
    Component { needs: &[&EYES], on: |run| On::when(config::method(run.options) == StereoMethod::Afw, "AER chosen"), ..afw12::frame_warping() };

/// AFW with frame generation: in-between pairs of the warped pairs.
pub const AFW_FRAMEGEN: Component<Beast> = Component { needs: &[&AFW, &FRAMEGEN], ..afw12::frame_generation() };

/// Each eye's depth to the headset with its pair (`headset_depth`, on by default): the present
/// loop's own pairs (AER) take it from the frame inputs, frame warping and frame generation
/// send their own.
pub const DEPTH: Component<Beast> = Component { needs: &[&EYES], ..depth_layer::depth_layer() };

/// Each eye's inputs for the features that ask (frame warping, frame generation, depth to the
/// headset, foveation's render size), from the Streamline calls [`STREAMLINE`] hooks or the
/// FSR dispatches [`FSR`] hooks (the frame inputs provider: [`providers::stereo`]).
pub const INPUTS: Component<Beast> = frame_inputs();

/// Foveated rendering (`monaka_stereo::foveate12`), on by default, with stereo and DLSS's inputs,
/// from what The Beast provides ([`providers::stereo`]): each frame's eye and head pose from its
/// label at DLSS's camera constants, each eye's frustum the one its camera is rendered with, and
/// the scene's passes those at the render size DLSS upscales from (its tagged inputs). Its command
/// lists come from the game queue (`Beast`'s `GameQueue`), shared with the HUD layer
/// (`monaka_stereo::lists12`); the game records on Streamline's proxy lists, so VRS goes on the
/// native lists behind them, found through the Streamline imports [`STREAMLINE`] hooks.
pub const FOVEATION: Component<Beast> = Component { needs: &[&EYES, &STREAMLINE], ..foveate12::foveation() };

// ---- Look ----

/// Head aim on the character (`monaka_body::look`, its targets delivery on The Beast's look fields:
/// [`providers::look_fields`]), steered at the arms visual's camera-target callback, which has the
/// character (the arms' `head_aim_here`; the camera setter only sees the renderer's camera, where
/// the game's own camera is noted before the eye camera replaces it).
pub const LOOK: Component<Beast> = Component { needs: &[&EYES], ..monaka_body::look::look() };

/// Without head aim, vertical look passes only back toward level (`monaka_body::pitch_lock`, at the
/// input-action converter: [`providers::look_fields`]).
pub const PITCH_LOCK: Component<Beast> = Component { needs: &[&EYES], ..monaka_body::pitch_lock::pitch_lock() };

/// The view stays level while head aim steers (`eng_chr::providers::view_bob`).
pub const VIEW_BOB: Component<Beast> = Component { needs: &[&LOOK], ..eng_chr::feature::view_bob() };

// ---- The arms ----

/// The arms on the controllers (`eng_chr::feature::hand_rig`, on The Beast's rig:
/// [`providers::hand_rig`]), at the callback head aim steers in.
pub const HAND_RIG: Component<Beast> = Component { needs: &[&LOOK], ..eng_chr::feature::hand_rig() };

/// A free hand's fingers on the tracked fingers (the hand rig's, at the arms' pose).
pub const FINGERS: Component<Beast> = Component { needs: &[&HAND_RIG], ..monaka_body::feature::fingers() };
pub const MELEE: Component<Beast> = Component { needs: &[&HAND_RIG], ..eng_chr::feature::melee() };
/// The bow drawn and loosed by the hands (`monaka_body::bow`, on [`providers::arms`]).
pub const BOW: Component<Beast> = Component { needs: &[&HAND_RIG], ..eng_chr::feature::bow() };
/// A throwable leaves the left hand as it threw it (`monaka_body::throw`, on [`providers::arms`]).
pub const THROW: Component<Beast> = Component { needs: &[&HAND_RIG], ..eng_chr::feature::throw() };
/// The left hand to the gun and away reloads (`monaka_body::reload`).
pub const RELOAD_BY_HAND: Component<Beast> = Component { needs: &[&HAND_RIG], ..monaka_body::reload::reload() };
pub const ROOM_SCALE: Component<Beast> = Component { needs: &[&HAND_RIG], ..monaka_body::feature::room_scale() };

/// Zombies' grabs never land on the player (`eng_chr::providers::grabs`).
pub const GRABS: Component<Beast> = eng_chr::feature::grabs();

// ---- The pad ----

/// The actions provider ([`providers::actions`]): ready-mades press the game's actions by its
/// bindings.
pub const ACTIONS: Component<Beast> = monaka_pad::feature::actions();

/// The tools (left) and weapons (right) wheels stay open while the d-pad is held, picked with A, B,
/// X, Y; the viewer holds them for a hand reached over that shoulder (its default). Part of the
/// controls, not a setting.
const DPAD_HOLD: Opt = dpad_hold(&["--dpad-hold", "left,right"]).on(Shown::Line);
/// The wheels pick with the right stick (not the face buttons): the reaching hand's flick goes to
/// the game as that stick.
const MENU_PICK_STICK: Opt = line("menu_pick_stick", "Wheels picked by stick", "The reaching hand's flick reaches the game as its right stick.", Kind::Switch { default: true })
    .launcher()
    .viewer_args("1", &["--menu-pick", "stick"]);

/// The controllers as the game's pad, the whole right stick to the game (its look steers toward
/// targets; head aim neutralises the stick's pitch itself), with the wheels held open by the d-pad.
pub const GAMEPAD: Component<Beast> = Component {
    options: &[CONTROLLER_PAD, DPAD_HOLD, MENU_PICK_STICK, CONTROLLER_PROXY],
    needs: &[&EYES],
    arm: monaka_pad::feature::whole_stick,
    ..monaka_pad::feature::gamepad()
};

// ---- The upscaler per eye, and the upscalers' inputs ----

/// The upscaler per eye is off unless asked for: it is still being tried.
const PER_EYE_DEFAULT: bool = false;

/// The game's upscaler per eye (`upscale_per_eye`, `monaka_stereo::upscaler`), DLSS or FSR,
/// whichever the game runs. DLSS: each eye its own DLSS viewport, so its own history, its constants
/// describing that eye's own previous frame and its motion vectors carried onto it
/// (`monaka_streamline::per_eye`, set up by [`STREAMLINE`] after this). FSR: a twin of the game's
/// upscale context for the right eye, the motion vectors chained onto each eye's previous frame
/// (`monaka_framegen::upscale_per_eye`, set up by [`FSR`]). With `dlss_jitter_per_eye` (default on)
/// each eye walks the renderer's whole jitter sequence ([`dlss`]); `dlss_fix_motion` (default on)
/// carries DLSS's motion vectors. Every rendering mode runs with it.
pub const PER_EYE: Component<Beast> = Component {
    name: "dlss per eye",
    options: &[UPSCALE_PER_EYE.default_switch(PER_EYE_DEFAULT)],
    needs: &[&EYES],
    install: dlss::install_per_eye,
    give_back: monaka_streamline::per_eye::give_back,
    ..monaka_stereo::upscaler::per_eye()
};

/// The renderer's Streamline imports ([`dlss`]): with stereo, DLSS's camera constants label each
/// frame with its eye; the frame inputs ([`INPUTS`]) and per-eye DLSS ([`PER_EYE`]) take theirs
/// there. Experiment: `dlss_reset=1` drops DLSS's history every frame.
pub const STREAMLINE: Component<Beast> = Component {
    name: "streamline",
    on: dlss::streamline_on,
    install: dlss::install_streamline,
    every: &[Every::millis(2000, monaka_streamline::imports::report)],
    release: monaka_streamline::imports::release,
    ..Component::NONE
};

/// The renderer's FidelityFX imports ([`fsr`]): with stereo, each FSR upscale labels its frame with
/// its eye; the frame inputs and FSR per eye ([`PER_EYE`], when on) take theirs there.
pub const FSR: Component<Beast> = Component {
    name: "fsr",
    on: |run| On::when(stereo_on(run.options), "no FSR inputs needed"),
    install: fsr::install_fsr,
    ..Component::NONE
};

// ---- The HUD ----

/// The HUD as a layer laid over each eye (`eng_chr::feature`'s ready-made; the lay-over at each
/// present is [`present12`]'s, at the HUD's size and distance the layer offers).
pub const HUD: Component<Beast> = Component { needs: &[&EYES], ..eng_chr::feature::hud() };
/// The HUD's pieces on the hands (`eng_chr::feature`'s ready-made).
pub const PANELS: Component<Beast> = Component { needs: &[&HUD], ..eng_chr::feature::panels() };

/// The research probes: their hooks and reports. Each probe checks its own option.
pub const PROBES: Component<Beast> = Component {
    install: research::install_probes,
    every: &[Every::millis(50, research::poll), Every::millis(2000, research::periodic)],
    release: research::report,
    ..monaka_producer::game::probes()
};
