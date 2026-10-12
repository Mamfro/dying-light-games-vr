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
//! The adapter ([`Dl2::ADAPTER`]), in the order its components start:
//!
//! - [`EYES`]: the stereo pair on the engine side (`view::scene`) and the eyes leaving the
//!   renderer (`output`);
//! - [`LOOK`]: head aim on the character at the player camera update; [`PITCH`]: the vertical look
//!   input held for head aim, or locked toward level (`lock_pitch`);
//! - [`ARMS`]: the arms on the controllers; [`FINGERS`]: their fingers; [`MELEE`], [`HANDWORK`],
//!   [`ROOM_SCALE`]: what the hands and steps do; [`VIEW_BOB`], [`GRABS`]: the game's own, for VR;
//! - [`GAMEPAD`]: the controllers as the game's pad (Monaka's);
//! - [`HUD`]: the HUD as a layer laid over each eye (D3D12); [`PANELS`]: its pieces on the hands;
//!   [`MARKERS`]: its world markers at their targets' depth;
//! - research: [`PROBES`].

mod config;
mod engine;
mod game;
mod hud;
mod manifest;
#[cfg(test)]
mod launch_tests;
mod output;
mod player;
mod research;
mod view;

use config::Config;
use eng_chr::feature::{Chrome, Facts as ChromeFacts};
use hud::markers;
use monaka_arms::aim::AimSettings;
use monaka_channel::pose::PoseChannel;
use monaka_core::feature::Feature;
use monaka_core::manifest::{
    CONTROLLER_PAD, CONTROLLER_PROXY, HUD_SIZE, Kind, Manifest, Opt, Shown, line, opt, render_size,
};
use monaka_core::options::Options;
use monaka_hook::InFlight;
use monaka_hook::module::Module;
use eng_chr::feature::gamedll;
use monaka_producer::game::{Adapter, Component, Game, Lack, Missing, On, Start};
use monaka_producer::{Rejection, Session, log, require_build};
use output::{framegen, render11, render12, video};
use player::{aim, hands};
use std::sync::atomic::Ordering;
use view::{head, scene};
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
            &EYES,
            &LOOK,
            &PITCH,
            &ARMS,
            &FINGERS,
            &MELEE,
            &HANDWORK,
            &ROOM_SCALE,
            &VIEW_BOB,
            &GRABS,
            &GAMEPAD,
            &HUD,
            &PANELS,
            &MARKERS,
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
        scene::publishing()
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

const MODE: Opt = opt("mode", "Rendering", "Smooth renders each eye and adds in-between frames, Sharp renders each eye, Fast draws once for both eyes (gaps beside near things).", Kind::Choice {
    choices: &[("framegen", "Smooth"), ("same", "Sharp"), ("depth", "Fast"), ("alternate", "Alternate eye"), ("auto", "Smooth")],
    default: "framegen",
    window: 3,
})
.on(Shown::Engine)
// Fast shows the hands no nearer than its nearest comfortable depth: the panels on them too.
.viewer_args("depth", &["--panel-nearest", "0.5"]);

/// Stereo in the mode the run picked (`config`): the pair on the engine side, each eye's camera and
/// frustum written into the scene (`view::scene`), the head from the viewer or synthetic
/// (`view::head`); the eyes leave the renderer at its present (`output`), at the headset's eye size
/// (`output::video`). Always on: menus and loading go out mono.
pub const EYES: Component<Dl2> = Component {
    name: "eyes",
    features: &[Feature::Stereo, Feature::PerEyeHistory],
    options: &[render_size("Experimental in this game."), MODE],
    needs_channel: true,
    settings: |options, manifest| {
        Config::from_options(options, manifest);
    },
    install: install_eyes,
    arm: start_publishing,
    // A pair in progress restores the camera itself before returning.
    quiet: || {
        scene::PUBLISHING.store(false, Ordering::Release);
        scene::stop_stereo();
    },
    give_back: || {
        video::restore();
        InFlight::wait_idle(2000);
        scene::abandon_pair();
    },
    release: || {
        framegen::close();
        render11::close();
        render12::close();
        head::HEAD.close();
        output::report();
    },
    summary: Some(scene::timing_report),
    ..Component::NONE
};

fn install_eyes(start: &mut Start<'_, Dl2>) -> Result<(), Lack> {
    let Facts { engine, renderer, d3d11, .. } = start.build;
    let channel = start.channel()?;
    // SAFETY: the engine's and the renderer's hashes were checked.
    unsafe {
        scene::install(start.hooks, engine)?;
        output::install(start.hooks, renderer, *d3d11, channel)?;
    }
    // A synthetic head still writes pose records (the viewer and the probe read them).
    head::open(PoseChannel::open(channel).map_err(|e| log!("pose channel: {e}; publishing mono")).ok());
    Ok(())
}

fn start_publishing() {
    scene::PUBLISHING.store(true, Ordering::Release);
    let config = config::get();
    let world = config.hud.world && game::try_get().is_some_and(|g| g.renderer12.is_some());
    let mode = match (config.framegen, config.depth_stereo, config.alternate_eye) {
        (true, _, _) => "same-frame stereo with FSR frame generation per eye",
        (_, true, _) => "depth stereo (one render, both eyes from depth)",
        (_, _, true) => "alternate-eye stereo",
        _ => "same-frame stereo",
    };
    let pose = if config.synthetic.is_some() {
        "a synthetic pose"
    } else if head::HEAD.connected() {
        "the viewer"
    } else {
        "nowhere (mono)"
    };
    log!("DL2 producer running: {mode}{}; head pose from {pose}", if world { " with the dynamic HUD" } else { "" });
}

// ---- Look ----

/// Head aim on the character (`player::aim`), steered at the player camera update
/// (`FromForwardUpPos`); with pointing aim the controllers come in. Handed back at the next update.
pub const LOOK: Component<Dl2> = Component { needs: &[&EYES], install: install_look, give_back: || aim::release(500), ..eng_chr::feature::look() };

fn install_look(start: &mut Start<'_, Dl2>) -> Result<(), Lack> {
    let gamedll = gamedll(start)?;
    let engine_module = &start.build.engine;
    let settings = AimSettings::from_options(start.options, start.shared.aim, true);
    aim::AIM.configure(settings, gamedll.base());
    start.hooks.site(&aim::FROM_FORWARD, engine_module, &engine::FROM_FORWARD_UP_POS, aim::from_forward)?;
    if settings.hand_aim {
        monaka_arms::controllers::open(start.channel()?);
    }
    Ok(())
}

const LOCK_PITCH: Opt = line("lock_pitch", "Block stick pitch", "With mouse / stick aim: no looking up and down with the stick, except toward level.", Kind::Switch { default: false });

/// The game's input-action converter (`player::aim`): while head aim steers, the head alone pitches
/// the character; without it, `lock_pitch` lets vertical look through only toward level.
pub const PITCH: Component<Dl2> = Component {
    name: "pitch",
    options: &[LOCK_PITCH],
    needs: &[&EYES],
    settings: |options, _| {
        options.switch("lock_pitch", false);
    },
    on: |run| On::when(run.shared.aim.head || run.options.switch("lock_pitch", false), "vertical look as the game has it"),
    install: install_pitch,
    ..Component::NONE
};

fn install_pitch(start: &mut Start<'_, Dl2>) -> Result<(), Lack> {
    let gamedll = gamedll(start)?;
    aim::pitch_actions_match(&start.build.engine).missing()?;
    start.hooks.site(&aim::INPUT_ACTION, gamedll, &engine::INPUT_ACTION, aim::input_action)?;
    Ok(())
}

// ---- Hand rig ----

/// The arms on the controllers at the arms visual's camera-target callback (`player::hands`,
/// `eng_chr::rig`), a swing of a held weapon or a fist attacking (`swing_attack`, `swing_speed`).
pub const ARMS: Component<Dl2> = Component { needs: &[&LOOK], install: |start| install_hands(start, true), release: hands::report, ..eng_chr::feature::arms() };

fn install_hands(start: &mut Start<'_, Dl2>, rig: bool) -> Result<(), Lack> {
    let gamedll = gamedll(start)?;
    let options = eng_chr::rig::Options::read(start.options, start.shared, rig, research::options().hands);
    // SAFETY: the game DLL's hash was checked.
    unsafe { hands::install(start.hooks, &start.build.engine, gamedll, options) }.missing()
}

/// A free hand's fingers on the tracked fingers (`eng_chr::fingers`, at the arms' pose).
pub const FINGERS: Component<Dl2> = Component { needs: &[&ARMS], ..monaka_arms::feature::fingers() };

pub const MELEE: Component<Dl2> = Component { needs: &[&ARMS], ..eng_chr::feature::melee() };
pub const HANDWORK: Component<Dl2> = Component { needs: &[&ARMS], ..eng_chr::feature::handwork() };
pub const ROOM_SCALE: Component<Dl2> = Component { needs: &[&ARMS], ..eng_chr::feature::room_scale() };

// ---- The game's own, for VR ----

pub const VIEW_BOB: Component<Dl2> = Component { needs: &[&LOOK], ..eng_chr::feature::view_bob() };
pub const GRABS: Component<Dl2> = eng_chr::feature::grabs();

// ---- Pad ----

/// The controllers as the game's pad, the whole right stick to the game (head aim and the pitch
/// lock decide what its pitch does).
pub const GAMEPAD: Component<Dl2> = Component { options: &[CONTROLLER_PAD, CONTROLLER_PROXY], arm: monaka_pad::feature::whole_stick, ..monaka_pad::feature::gamepad() };

// ---- HUD ----

pub const HUD: Component<Dl2> = Component { options: &[HUD_SIZE.default_number(0.55)], needs: &[&EYES], ..eng_chr::feature::hud() };
pub const PANELS: Component<Dl2> = Component { needs: &[&HUD], ..eng_chr::feature::panels() };

const WORLD_MARKERS: Opt =
    opt("world_markers", "Markers in the world", "Objective and waypoint markers sit at their targets' distance instead of in the flat HUD.", Kind::Switch { default: true }).on(Shown::Hud);

/// The HUD's world markers (objectives, waypoints) drawn at their targets' depth with the dynamic
/// HUD (`hud::markers`: their world points each tick).
pub const MARKERS: Component<Dl2> = Component {
    name: "markers",
    options: &[WORLD_MARKERS],
    needs: &[&PANELS],
    on: |run| On::when(run.shared.hud.markers, "markers stay in the flat HUD"),
    install: |start| {
        // SAFETY: the game DLL's hash was checked.
        unsafe { markers::install(start.hooks, gamedll(start)?) }.missing()
    },
    arm: || markers::enable(true),
    quiet: || markers::enable(false),
    release: markers::report,
    ..Component::NONE
};

// ---- Research ----

/// The research probes: their hooks and reports. Each probe checks its own option; the arms and
/// markers probes take their component's hook when it is off.
pub const PROBES: Component<Dl2> = Component { install: install_probes, release: research::report, ..monaka_producer::game::probes() };

fn install_probes(start: &mut Start<'_, Dl2>) -> Result<(), Lack> {
    let probes = research::options();
    let Some(gamedll) = start.build.gamedll.as_ref() else { return Ok(()) };
    if probes.hands
        && !start.is_on(&ARMS)
        && let Err(Lack::Missing(why)) = install_hands(start, false)
    {
        log!("arms probe off: {why}");
    }
    if probes.markers && !start.is_on(&MARKERS) {
        // SAFETY: the game DLL's hash was checked.
        if let Err(why) = unsafe { markers::install(start.hooks, gamedll) } {
            log!("markers probe off: {}", why.reason);
        }
    }
    if (probes.melee || probes.sweep)
        && let Err(why) = research::melee::install(start.hooks, gamedll, probes.melee, probes.sweep)
    {
        log!("melee probe off: {}", why.reason);
    }
    Ok(())
}
