//! Dying Light: The Beast (D3D12) in head-tracked stereo, injected by `monaka_loader`. Options come
//! from `beast-options.txt` beside the DLL copy; the research probes' are in `research`.
//!
//! The adapter ([`Beast::ADAPTER`]), in the order its components start:
//!
//! - [`CAMERAS`]: the renderer camera's two exported setters, where the player camera update is
//!   taken over (`view::cameras`); [`PRESENT`]: the D3D12 present request every frame goes through;
//! - [`EYE_SIZE`]: the game rendering at the headset's eye size; [`EYES`]: stereo by alternate eyes,
//!   the player camera set to each eye in turn, each frame labelled by its camera;
//! - [`LOOK`]: head aim on the character, steered at the arms visual's callback; [`ARMS`]: the arms
//!   on the controllers there; [`FINGERS`]: their fingers; [`MELEE`], [`HANDWORK`], [`ROOM_SCALE`]:
//!   what the hands and steps do; [`VIEW_BOB`], [`GRABS`], [`FLASHLIGHT`]: the game's own, for VR;
//! - [`GAMEPAD`]: the controllers as the game's pad (Monaka's);
//! - [`STREAMLINE`]: DLSS's inputs (the frames' labels, per-eye experiments);
//! - [`HUD`]: the HUD as a layer laid over each eye; [`PANELS`]: its pieces on the hands;
//! - [`PROBES`]: research; [`REPORT`]: the run's counters every two seconds.

pub mod engine;
mod manifest;
#[cfg(test)]
mod launch_tests;
mod output;
mod player;
mod research;
mod view;

use eng_chr::feature::{Chrome, Facts as ChromeFacts};
use monaka_arms::aim::AimSettings;
use monaka_core::feature::Feature;
use monaka_core::manifest::{
    CONTROLLER_PAD, CONTROLLER_PROXY, EYE_SEPARATION, Kind, LATENCY, Manifest, Opt, Shown, choice, dpad_hold, line, opt, render_size,
};
use monaka_core::options::Options;
use monaka_core::sync::Locked;
use monaka_hook::module::Module;
use monaka_producer::game::{Adapter, Component, Game, Lack, On, Start, on, place};
use monaka_producer::{Rejection, Session, log, require_build};
use output::{dlss, framegen, present12, streamline, video};
use player::{aim, flashlight, hands};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use view::{camera_update, cameras, stereo};
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
            &PRESENT,
            &EYE_SIZE,
            &EYES,
            &LOOK,
            &ARMS,
            &FINGERS,
            &MELEE,
            &HANDWORK,
            &ROOM_SCALE,
            &VIEW_BOB,
            &GRABS,
            &FLASHLIGHT,
            &GAMEPAD,
            &STREAMLINE,
            &HUD,
            &PANELS,
            &PROBES,
            &REPORT,
        ],
    };
    type Build = Facts;
    type Frame = ();

    fn prepare(_: &'static Session, _: &'static Manifest, options: &Options) -> Result<Facts, Rejection> {
        let renderer = require_build(engine::RENDERER, engine::RENDERER_SHA256)?;
        log!("{} at {:#x}", engine::RENDERER, renderer.base());
        let engine_module = Module::find(engine::ENGINE).ok_or_else(|| Rejection::not_ready("the engine is not loaded"))?;
        let stereo = stereo_on(options);
        let gamedll = if stereo || turn_yaw(options).is_some() {
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
        present12::game_queue()
    }

    fn showing() -> bool {
        stereo::running()
    }
}

/// Stereo (`stereo=0`: the probes only, the game as it plays on the monitor).
fn stereo_on(options: &Options) -> bool {
    options.switch("stereo", true)
}

/// `turn_yaw=<degrees>`: the player camera turned, the PC check that writing it reaches the
/// picture (`camera_update`).
fn turn_yaw(options: &Options) -> Option<f32> {
    options.angle("turn_yaw", 90.0).filter(|&yaw| yaw != 0.0)
}

/// The game DLL, which every component on the character needs (there with stereo).
fn gamedll<'a>(start: &Start<'a, Beast>) -> Result<&'a Module, Lack> {
    start.build.gamedll.as_ref().ok_or_else(|| Lack::Missing("the game DLL is not hooked without stereo".to_owned()))
}

/// The components' places, for the report to ask whether each is on.
const PRESENT_AT: usize = place::<Beast>(&PRESENT);
const EYES_AT: usize = place::<Beast>(&EYES);
const STREAMLINE_AT: usize = place::<Beast>(&STREAMLINE);

// ---- The frame ----

/// The renderer camera's exported setters: the player camera update passes `SetView(vec3)`, where
/// the eye camera, head aim and the turn check take over (`view::cameras`), and
/// `ComputeFrustumMatrix`, where the headset's field of view goes in. Always on: the probes ride on
/// them too.
pub const CAMERAS: Component<Beast> = Component { name: "cameras", install: install_cameras, quiet: camera_update::disable, ..Component::NONE };

fn install_cameras(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let Facts { renderer, gamedll, .. } = start.build;
    if let Some(gamedll) = gamedll {
        camera_update::set_player_update(gamedll.at(engine::PLAYER_CAMERA_UPDATE));
    }
    let (vectors, frustum) = (engine::SET_VIEW_VECTORS, engine::COMPUTE_FRUSTUM);
    // SAFETY: each target is the named export (checked) of the function type its original holds,
    // and its prologue is checked byte for byte before anything is written.
    unsafe {
        start.hooks.inline(&cameras::SET_VECTORS_ORIGINAL, "CCamera::SetView(vec3)", cameras::export(renderer, vectors)?, vectors.2, cameras::set_vectors as engine::SetVectorsFn)?;
        start.hooks.inline(&cameras::COMPUTE_FRUSTUM_ORIGINAL, "CCamera::ComputeFrustumMatrix", cameras::export(renderer, frustum)?, frustum.2, cameras::compute_frustum_detour())?;
    }
    if let Some(yaw) = turn_yaw(start.options) {
        camera_update::enable_turn(yaw);
    }
    Ok(())
}

/// The D3D12 present request: every frame's eye kept or its pair published, the HUD laid over it,
/// the next eye chosen (`output::present12`).
pub const PRESENT: Component<Beast> = Component { name: "present", install: install_present, ..Component::NONE };

fn install_present(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let Some(r12) = &start.build.renderer_12 else { return Err(Lack::Missing("the game is not on D3D12".to_owned())) };
    present12::set_renderer(r12.base());
    let (rva, prologue) = engine::PRESENT_REQUEST_12;
    // SAFETY: the present request (prologue checked byte for byte) takes the request object.
    unsafe { start.hooks.inline(&present12::PRESENT_REQUEST, "present request", r12.at(rva), prologue, present12::present_request as engine::PresentRequest12Fn)? };
    Ok(())
}

// ---- Stereo ----

const EYE_SIZE_OF_YOUR_OWN: Opt = line("eye_size", "Render size (WxH)", "A render size of your own; replaces render_size.", Kind::Size).launcher();

/// The game renders at the headset's eye size, windowed, while VR runs (`output::video`): asked for
/// once the hooks are in, publishing held until the game presents at it (the channel takes the
/// first published frame's size), so it starts before the eyes.
pub const EYE_SIZE: Component<Beast> = Component {
    name: "eye size",
    options: &[
        render_size("The headset's size runs much better.").eye_size_note("Click into the game: it switches to {size} once it has focus, and VR shows it from then on."),
        EYE_SIZE_OF_YOUR_OWN,
    ],
    needs: &[&PRESENT],
    on: |run| On::when(stereo_on(run.options) && run.options.size("eye_size").is_some(), "the game renders at its own size"),
    install: install_eye_size,
    arm: video::switch,
    give_back: video::restore,
    ..Component::NONE
};

fn install_eye_size(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let eye = start.options.size("eye_size").ok_or_else(|| Lack::Missing("no eye size".to_owned()))?;
    // SAFETY: the fingerprinted game DLL imports GetGameTimeDelta with this type.
    unsafe { eng_chr::game::hook(start.hooks, gamedll(start)?)? };
    video::want(start.build.renderer, eye);
    Ok(())
}

const STEREO: Opt = line("stereo", "Stereo", "Off: the probes only, the game as it plays on the monitor.", Kind::Switch { default: true });
const MODE: Opt = opt("mode", "Rendering", "Smooth adds in-between frames.", choice(&[("standard", "Standard"), ("framegen", "Smooth")], "standard")).on(Shown::Engine);

/// Smooth (`mode=framegen`): per-eye frame generation, its own pairs from the game's FFX loader
/// (not with the per-eye DLSS experiment).
fn framegen_on(options: &Options) -> bool {
    options.choice("mode", &[("framegen", true), ("standard", false)], options.switch("framegen", false)) && !options.switch("dlss_per_eye", false)
}

/// Stereo by alternate eyes (`view::stereo`): after each present the next eye and the head pose
/// are chosen, the player camera update sets the camera to that eye, and the frame is labelled by
/// its camera as DLSS gets it; `latency` presents later it goes out with its partner eye.
/// `eye_test=` and `latency_test=` (degrees) turn the eyes or every other pair's head for PC checks.
pub const EYES: Component<Beast> = Component {
    name: "eyes",
    features: &[Feature::Stereo],
    options: &[MODE, STEREO, LATENCY, EYE_SEPARATION],
    needs: &[&PRESENT],
    needs_channel: true,
    settings: |options, _| {
        stereo_on(options);
        framegen_on(options);
    },
    on: |run| On::when(stereo_on(run.options), "stereo is off: the probes only"),
    install: install_eyes,
    arm: stereo::arm,
    quiet: || stereo::quiet(1000),
    release: || {
        framegen::close();
        stereo::release();
    },
    ..Component::NONE
};

fn install_eyes(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let (options, shared, channel) = (start.options, start.shared, start.channel()?);
    if framegen_on(options) {
        match monaka_framegen::load() {
            Some(api) => {
                framegen::enable(api);
                framegen::set_channel(channel.clone());
                log!("FSR frame generation per eye: on");
            }
            None => log!("FSR frame generation unavailable ({} not loadable); alternate-eye without it", monaka_framegen::ffx::LOADER),
        }
    }
    stereo::install(stereo::Settings {
        channel: channel.clone(),
        schedule: shared.schedule,
        separation: shared.eye_separation,
        synthetic: shared.synthetic_head().map(|pose| Box::new(move || pose) as monaka_channel::pose::Synthetic),
        eye_test_degrees: options.angle("eye_test", 30.0).unwrap_or(0.0),
        latency_test_degrees: options.angle("latency_test", 30.0).unwrap_or(0.0),
        headset_fov: shared.headset_fov,
        hud: shared.hud.shown.then_some((shared.hud.scale, shared.hud.distance)),
    });
    Ok(())
}

// ---- Look and the hand rig ----

/// Head aim on the character (`player::aim`): steered at the arms visual's camera-target callback,
/// which has the character (`player::hands`); while it steers, the head alone pitches the view (the
/// input action converter). With pointing aim the controllers come in. Handed back at the next
/// camera update.
pub const LOOK: Component<Beast> = Component {
    needs: &[&EYES],
    install: install_look,
    give_back: || aim::release(500),
    release: || {
        hands::report();
        monaka_arms::controllers::close();
    },
    ..eng_chr::feature::look()
};

fn install_look(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let gamedll = gamedll(start)?;
    let settings = AimSettings::from_options(start.options, start.shared.aim, true);
    aim::configure(settings, gamedll.base());
    log!("{settings:?}");
    match aim::pitch_actions_match(&start.build.engine) {
        // SAFETY: the converter's exact prologue is checked by the hooker; the detour has its type.
        Ok(()) => unsafe {
            let (rva, prologue) = engine::INPUT_ACTION;
            start.hooks.inline(&aim::INPUT_ACTION, "input action", gamedll.at(rva), prologue, aim::input_action as engine::InputActionFn)?;
        },
        Err(why) => log!("vertical look not locked: {why}"),
    }
    // SAFETY: the game DLL's hash was checked.
    unsafe { hands::hook(start.hooks, gamedll) }.map_err(|why| Lack::Missing(format!("arms callback: {}", why.reason)))?;
    if settings.hand_aim {
        monaka_arms::controllers::open(start.channel()?);
    }
    Ok(())
}

/// The arms on the controllers at the arms visual's callback (`eng_chr::rig`), a swing of a held
/// weapon or a fist attacking (`swing_attack`, `swing_speed`).
pub const ARMS: Component<Beast> = Component {
    needs: &[&LOOK],
    install: |start| hands::configure(&start.build.engine, hands_settings(start, true)).map_err(|why| Lack::Missing(why.reason)),
    ..eng_chr::feature::arms()
};

fn hands_settings(start: &Start<'_, Beast>, rig: bool) -> hands::Settings {
    hands::Settings::read(start.options, start.shared, rig, research::options().hands)
}

/// A free hand's fingers on the tracked fingers (`eng_chr::fingers`, at the arms' pose).
pub const FINGERS: Component<Beast> = Component { needs: &[&ARMS], ..monaka_arms::feature::fingers() };

pub const MELEE: Component<Beast> = Component { needs: &[&ARMS], ..eng_chr::feature::melee() };
pub const HANDWORK: Component<Beast> = Component { needs: &[&ARMS], ..eng_chr::feature::handwork() };
pub const ROOM_SCALE: Component<Beast> = Component { needs: &[&ARMS], ..eng_chr::feature::room_scale() };

// ---- The game's own, for VR ----

pub const VIEW_BOB: Component<Beast> = Component { needs: &[&LOOK], ..eng_chr::feature::view_bob() };
pub const GRABS: Component<Beast> = eng_chr::feature::grabs();

/// A steady flashlight (`player::flashlight`): its sway held at zero and its screen-space shadows
/// off, which in VR swing with the head.
pub const FLASHLIGHT: Component<Beast> = Component {
    name: "flashlight",
    needs: &[&EYES],
    on: |run| On::when(flashlight::Settings::from_options(run.options).any(), "the flashlight as in the game"),
    install: |start| flashlight::install(start.hooks, &start.build.engine, flashlight::Settings::from_options(start.options)).map_err(|why| Lack::Missing(why.reason)),
    release: flashlight::report,
    ..Component::NONE
};

// ---- Pad ----

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
/// targets; head aim neutralises the stick's pitch itself).
pub const GAMEPAD: Component<Beast> = Component {
    options: &[CONTROLLER_PAD, DPAD_HOLD, MENU_PICK_STICK, CONTROLLER_PROXY],
    needs: &[&EYES],
    arm: monaka_pad::feature::whole_stick,
    ..monaka_pad::feature::gamepad()
};

// ---- DLSS ----

/// The renderer's Streamline imports (`output::streamline`): with stereo, DLSS's camera constants
/// label each frame with its eye; frame generation takes its inputs there. Experiments:
/// `dlss_reset=1` drops DLSS's history every frame, `dlss_per_eye=1` gives each eye its own DLSS
/// viewport (`dlss_jitter_per_eye`, default on: its jitter too), `dlss_fix_motion` (default on)
/// fixes the motion vectors DLSS gets.
pub const STREAMLINE: Component<Beast> = Component {
    name: "streamline",
    on: |run| {
        let wanted = stereo_on(run.options) || research::options().streamline || run.options.switch("dlss_reset", false);
        On::when(wanted, "no DLSS inputs needed")
    },
    install: install_streamline,
    give_back: || dlss::ENABLED.store(false, Ordering::Relaxed),
    release: streamline::release,
    ..Component::NONE
};

fn install_streamline(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let Some(r12) = &start.build.renderer_12 else { return Err(Lack::Missing("the game is not on D3D12".to_owned())) };
    let options = start.options;
    let reset = options.switch("dlss_reset", false);
    let per_eye = stereo_on(options) && options.switch("dlss_per_eye", false);
    dlss::FIX_MOTION.store(options.switch("dlss_fix_motion", true), Ordering::Relaxed);
    if per_eye && options.switch("dlss_jitter_per_eye", true) {
        let jitter = engine::FRAME_JITTER;
        // SAFETY: the named export (checked) of type FrameJitterFn, prologue checked byte for byte.
        unsafe { start.hooks.inline(&dlss::FRAME_JITTER, "CShaderCamera::GetFrameJitter", cameras::export(&start.build.renderer, jitter)?, jitter.2, dlss::frame_jitter as engine::FrameJitterFn)? };
        dlss::JITTER_PER_EYE.store(true, Ordering::Relaxed);
    }
    // SAFETY: rd3d12 is the game's D3D12 renderer DLL.
    unsafe { streamline::install(start.hooks, r12, reset, per_eye, framegen::enabled()) };
    research::motion::arm();
    Ok(())
}

// ---- HUD ----

pub const HUD: Component<Beast> = Component { needs: &[&EYES], ..eng_chr::feature::hud() };
pub const PANELS: Component<Beast> = Component { needs: &[&HUD], ..eng_chr::feature::panels() };

// ---- Research and the run's report ----

/// The research probes: their hooks on the renderer and the game DLL. Each probe checks its own
/// option; `probe_hands` rides on the look's arms callback.
pub const PROBES: Component<Beast> = Component { install: install_probes, release: research::report, ..monaka_producer::game::probes() };

fn install_probes(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let stereo = stereo_on(start.options);
    if research::options().hands
        && start.is_on(&LOOK)
        && !start.is_on(&ARMS)
        && let Err(why) = hands::configure(&start.build.engine, hands_settings(start, false))
    {
        log!("arms probe off: {}", why.reason);
    }
    // SAFETY: the renderer's and the game DLL's hashes were checked.
    unsafe {
        research::install(start.hooks, &start.build.renderer, stereo)?;
        research::install_game(start.hooks, start.build.gamedll.as_ref())?;
    }
    research::start(stereo, start.build.renderer.base());
    Ok(())
}

static REPORTER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static STOPPING: AtomicBool = AtomicBool::new(false);
const REPORT_EVERY: Duration = Duration::from_secs(2);

/// The run's counters every two seconds (presents, stereo, DLSS's inputs, frame generation) and the
/// probes' scans and reports, on a thread of its own.
pub const REPORT: Component<Beast> = Component { name: "report", arm: start_reporter, quiet: stop_reporter, ..Component::NONE };

fn start_reporter() {
    STOPPING.store(false, Ordering::Release);
    let reporter = std::thread::Builder::new().name("monaka_beast-report".into()).spawn(|| {
        let started = Instant::now();
        let mut last = started;
        while !STOPPING.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(50));
            research::poll(started.elapsed());
            if last.elapsed() < REPORT_EVERY {
                continue;
            }
            let seconds = last.elapsed().as_secs_f64();
            if on::<Beast>(PRESENT_AT) {
                present12::report(seconds);
            }
            if on::<Beast>(EYES_AT) {
                stereo::report(seconds);
            }
            if on::<Beast>(STREAMLINE_AT) {
                streamline::report(seconds);
            }
            if framegen::enabled() {
                log!("{}", framegen::report());
            }
            research::periodic(seconds);
            last = Instant::now();
        }
    });
    match reporter {
        Ok(handle) => *REPORTER.locked() = Some(handle),
        Err(e) => log!("no reporter thread ({e})"),
    }
}

fn stop_reporter() {
    STOPPING.store(true, Ordering::Release);
    if let Some(reporter) = REPORTER.locked().take() {
        let _ = reporter.join();
    }
}
