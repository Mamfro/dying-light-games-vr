//! Dying Light: The Beast producer (`monaka_beast.dll`). Options come from `beast-options.txt`
//! beside the DLL copy (`config`, the manifest's options; the research probes' in `research`).
//!
//! Two exported `CCamera` methods are always hooked (the player camera update goes through one of
//! them, `cameras`), and on D3D12 the present request (`present12`). The probes are in `research`.

mod config;
mod manifest;
#[cfg(test)]
mod launch_tests;
mod output;
mod player;
mod research;
mod view;

use view::{camera_update, cameras, stereo};
use output::{dlss, framegen, present12, streamline, video};
use player::{aim, hands};
pub mod engine;

use config::Config;
use monaka_arms::melee;
use monaka_hook::Hooks;
use monaka_hook::module::Module;
use monaka_producer::{Rejection, Session, log};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

monaka_producer::producer!(manifests: crate::manifest::MANIFESTS, start: start, stop: stop);

static HOOKS: Mutex<Option<Hooks>> = Mutex::new(None);
static REPORTER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
/// The dynamic HUD runs (the pieces onto the hands at each present).
pub static WORLD_HUD: AtomicBool = AtomicBool::new(false);
static STOPPING: AtomicBool = AtomicBool::new(false);
const REPORT_EVERY: Duration = Duration::from_secs(2);

fn start(session: &'static Session) -> Result<(), Rejection> {
    monaka_producer::install_crash_log();
    let options = session.manifest_options(&manifest::MANIFESTS[0]);
    let config = Config::from_options(&options);
    log!("{config:?}");
    crate::research::configure(&options);
    let stereo = config.stereo;
    let channel = if stereo { Some(session.require_channel()?) } else { None };

    let renderer = monaka_producer::require_build(engine::RENDERER, engine::RENDERER_SHA256)?;
    log!("{} at {:#x}", engine::RENDERER, renderer.base());
    let gamedll = if stereo || config.turn_yaw.is_some() {
        let gamedll = monaka_producer::require_build(engine::GAMEDLL, engine::GAMEDLL_SHA256)?;
        let (call, bytes) = engine::PLAYER_CAMERA_CALL;
        if !gamedll.bytes_match(call, bytes) {
            return Err(Rejection::revision("the player camera update's call is not where it was inspected"));
        }
        camera_update::set_player_update(gamedll.at(engine::PLAYER_CAMERA_UPDATE));
        Some(gamedll)
    } else {
        None
    };
    // The D3D12 present request, when the game runs on D3D12.
    let renderer_12 = match Module::find(engine::RENDERER_12) {
        Some(_) => Some(monaka_producer::require_build(engine::RENDERER_12, engine::RENDERER_12_SHA256)?),
        None if stereo => return Err(Rejection::unsupported("stereo needs the game on DirectX 12")),
        None => {
            log!("{} is not loaded (D3D11?): no present probe", engine::RENDERER_12);
            None
        }
    };

    let target = |target: engine::Target| cameras::export(&renderer, target);
    let mut hooks = Hooks::default();
    // Head aim, hand aim and the arms on the controllers (with stereo, on the inspected game DLL).
    let aim_settings = config.arms_aim.only_if(stereo && gamedll.is_some());
    let (head_aim, hand_rig, aim_hand) = (aim_settings.head_aim, aim_settings.hand_rig, aim_settings.aim_hand);
    aim::configure(aim_settings, gamedll.as_ref().map_or(0, Module::base));
    log!("{aim_settings:?}");
    let probe_hands = stereo && crate::research::options().hands;
    if let (Some(gamedll), true) = (&gamedll, head_aim || probe_hands) {
        let engine_module = Module::find(engine::ENGINE).ok_or_else(|| Rejection::not_ready("the engine is not loaded"))?;
        if head_aim {
            match aim::pitch_actions_match(&engine_module) {
                // SAFETY: the converter's exact prologue is checked by the hooker; the detour has its type.
                Ok(()) => unsafe {
                    let (rva, prologue) = engine::INPUT_ACTION;
                    hooks.inline(&aim::INPUT_ACTION, "input action", gamedll.at(rva), prologue, aim::input_action as engine::InputActionFn)?;
                },
                Err(why) => log!("vertical look not locked: {why}"),
            }
        }
        {
            let settings = hands::Settings { rig: hand_rig, probe: probe_hands, melee: melee::Settings { aim_side: aim_hand, ..config.melee }, fingers: config.fingers.tracking };
            monaka_arms::fingers::set_binary(config.fingers.binary);
            // SAFETY: the game DLL's hash was checked above.
            if let Err(why) = unsafe { hands::install(&mut hooks, &engine_module, gamedll, settings) } {
                log!("arms hook off (no head aim, no hand rig): {}", why.reason);
            }
        }
        if hand_rig
            && let Some(physical) = config.physical_melee
            && let Err(why) = eng_chr::melee::install(&mut hooks, gamedll, engine::MELEE, physical)
        {
            log!("physical melee off: {}", why.reason);
        }
        if config.disable_zombie_grabs {
            eng_chr::grabs::enable();
        }
        if hand_rig && config.handwork != eng_chr::handwork::Options::default() {
            eng_chr::handwork::install(&mut hooks, gamedll, &engine::HANDWORK, config.handwork);
        }
        let flashlight = &config.flashlight;
        if (flashlight.steady || flashlight.source.is_some() || flashlight.shadow_offset.is_some() || flashlight.shadow_scale.is_some() || flashlight.probe)
            && let Err(why) = player::flashlight::install(&mut hooks, &engine_module, config.flashlight)
        {
            log!("flashlight left as it is: {}", why.reason);
        }
        if hand_rig
            && let Some(settings) = config.room_scale
        {
            match eng_chr::walk::install(&mut hooks, &engine_module, gamedll, engine::WALK) {
                Ok(()) => monaka_arms::roomscale::install(settings),
                Err(why) => log!("room-scale following off: {}", why.reason),
            }
        }
    }
    if let Some(channel) = channel.as_ref().filter(|_| aim_settings.hand_aim || hand_rig) {
        monaka_arms::controllers::open(channel);
    }
    // SAFETY: each target is the named export (checked above) of the function type its original
    // holds, and its prologue is checked byte for byte before anything is written.
    unsafe {
        hooks.inline(
            &cameras::SET_VECTORS_ORIGINAL,
            "CCamera::SetView(vec3)",
            target(engine::SET_VIEW_VECTORS)?,
            engine::SET_VIEW_VECTORS.2,
            cameras::set_vectors as engine::SetVectorsFn,
        )?;
        hooks.inline(
            &cameras::COMPUTE_FRUSTUM_ORIGINAL,
            "CCamera::ComputeFrustumMatrix",
            target(engine::COMPUTE_FRUSTUM)?,
            engine::COMPUTE_FRUSTUM.2,
            cameras::compute_frustum_detour(),
        )?;
    }
    let dlss_reset = config.dlss_reset;
    let dlss_per_eye = stereo && config.dlss_per_eye;
    // Per-eye frame generation (`mode=framegen`): its own pairs, from the game's FFX loader.
    let framegen = stereo && renderer_12.is_some() && !dlss_per_eye && config.framegen;
    if let (true, Some(channel)) = (framegen, channel.as_ref()) {
        match monaka_framegen::load() {
            Some(api) => {
                framegen::enable(api);
                framegen::set_channel(channel.clone());
                log!("FSR frame generation per eye: on");
            }
            None => log!("FSR frame generation unavailable ({} not loadable); alternate-eye without it", monaka_framegen::ffx::LOADER),
        }
    }
    dlss::FIX_MOTION.store(config.dlss_fix_motion, Ordering::Relaxed);
    if dlss_per_eye && config.dlss_jitter_per_eye {
        // SAFETY: the named export (checked) of type FrameJitterFn, prologue checked byte for byte.
        unsafe {
            hooks.inline(&dlss::FRAME_JITTER, "CShaderCamera::GetFrameJitter", target(engine::FRAME_JITTER)?, engine::FRAME_JITTER.2, dlss::frame_jitter as engine::FrameJitterFn)?;
        }
        dlss::JITTER_PER_EYE.store(true, Ordering::Relaxed);
    }
    // With stereo, DLSS's camera constants label each frame with its eye (`stereo::label_frame`).
    let streamline_hooked = renderer_12.is_some() && (stereo || crate::research::options().streamline || dlss_reset);
    if let Some(r12) = renderer_12.as_ref().filter(|_| streamline_hooked) {
        // SAFETY: rd3d12 is the game's D3D12 renderer DLL.
        unsafe { streamline::install(&mut hooks, r12, dlss_reset, dlss_per_eye, framegen::enabled()) };
        crate::research::motion::arm();
    }
    if let Some(r12) = &renderer_12 {
        present12::set_renderer(r12.base());
        let (rva, prologue) = engine::PRESENT_REQUEST_12;
        // SAFETY: the present request (prologue checked byte for byte) takes the request object.
        unsafe {
            hooks.inline(&present12::PRESENT_REQUEST, "present request", r12.at(rva), prologue, present12::present_request as engine::PresentRequest12Fn)?;
        }
        // The dynamic HUD (`world_hud=0` turns it off): the HUD as a layer of its own from the
        // game's command lists (`probe_hud=1`: its probe), the pieces' boxes from the engine's
        // gui tree (`probe_gui=1`: its probe), the pieces on the hands, out of the eyes.
        let (probe_hud, probe_gui) = (crate::research::options().hud, crate::research::options().gui);
        let world_hud = stereo && config.hud.world;
        if world_hud || probe_hud {
            match present12::game_queue() {
                // SAFETY: the game's live direct queue.
                Some(queue) => {
                    if let Err(e) = unsafe { eng_chr::hudlayer::install(&mut hooks, &queue, false, probe_hud, channel.clone()) } {
                        log!("hud layer off: {e}");
                    }
                }
                None => log!("hud layer off: the game queue is not known yet"),
            }
        }
        if world_hud || probe_gui {
            match Module::find(engine::ENGINE).ok_or_else(|| Rejection::not_ready("the engine is not loaded")).and_then(|engine_module| eng_chr::gui::install(&mut hooks, &engine_module, &engine::GUI_BUILD)) {
                Ok(()) => eng_chr::gui::configure(probe_gui, if world_hud { &engine::PIECES } else { &[] }, stereo::running),
                Err(why) => log!("gui tree unavailable (no dynamic HUD): {}", why.reason),
            }
        }
        WORLD_HUD.store(world_hud, Ordering::Relaxed);
    }
    if let Some(channel) = channel.as_ref().filter(|_| config.controller_pad) {
        monaka_pad::install_optional(&mut hooks, channel);
    }
    let stereo_settings = channel.map(|channel| stereo::Settings {
        channel,
        schedule: config.schedule,
        separation: config.eye_separation,
        synthetic: config.synthetic_head().map(|pose| Box::new(move || pose) as monaka_channel::pose::Synthetic),
        eye_test_degrees: config.eye_test_degrees,
        latency_test_degrees: config.latency_test_degrees,
        headset_fov: config.headset_fov,
    });
    if let (Some(_), Some(gamedll)) = (config.eye_size, &gamedll) {
        // SAFETY: the fingerprinted game DLL imports GetGameTimeDelta with this type.
        unsafe { eng_chr::game::hook(&mut hooks, gamedll)? };
    }
    // SAFETY: the renderer's hash was checked above.
    unsafe { crate::research::install(&mut hooks, &renderer, stereo)? };
    monaka_producer::enable_hooks(&HOOKS, hooks)?;
    // The headset's eye size: publishing waits for it (the stereo channel takes the first published
    // frame's size).
    if let Some(eye) = config.eye_size
        && let Err(why) = video::switch_to(&renderer, eye)
    {
        log!("rendering at the game's own size: {why}");
    }
    if let Some(settings) = stereo_settings {
        stereo::start(settings);
    }
    if let Some(yaw) = config.turn_yaw {
        camera_update::enable_turn(yaw);
    }
    STOPPING.store(false, Ordering::Release);
    let presents = renderer_12.is_some();
    crate::research::start(stereo, renderer.base());
    let reporter = std::thread::Builder::new().name("monaka_beast-report".into()).spawn(move || {
        let started = Instant::now();
        let mut last = started;
        while !STOPPING.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(50));
            crate::research::poll(started.elapsed());
            if last.elapsed() >= REPORT_EVERY {
                let seconds = last.elapsed().as_secs_f64();
                if presents {
                    present12::report(seconds);
                }
                if stereo {
                    stereo::report(seconds);
                }
                if streamline_hooked {
                    streamline::report(seconds);
                }
                if framegen::enabled() {
                    log!("{}", framegen::report());
                }
                crate::research::periodic(seconds);
                last = Instant::now();
            }
        }
    });
    match reporter {
        Ok(handle) => *REPORTER.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle),
        Err(e) => log!("no reporter thread ({e})"),
    }
    Ok(())
}

fn stop() -> Result<(), Rejection> {
    // The next player camera update hands the head's share of the look back.
    aim::release(500);
    monaka_arms::roomscale::stop();
    eng_chr::walk::report();
    eng_chr::handwork::report();
    eng_chr::grabs::report();
    player::flashlight::report();
    monaka_pad::stop();
    camera_update::disable();
    stereo::quiet(1000);
    video::restore();
    STOPPING.store(true, Ordering::Release);
    if let Some(reporter) = REPORTER.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = reporter.join();
    }
    dlss::ENABLED.store(false, Ordering::Relaxed);
    WORLD_HUD.store(false, Ordering::Relaxed);
    eng_chr::hudlayer::remove(1000);
    monaka_producer::remove_hooks(&HOOKS, 2000)?;
    eng_chr::panels::close();
    eng_chr::hudfix::close();
    eng_chr::gui::report();
    framegen::close();
    monaka_pad::report();
    monaka_pad::release();
    hands::report();
    monaka_arms::controllers::close();
    streamline::release();
    stereo::release();
    crate::research::report();
    monaka_producer::remove_crash_log();
    Ok(())
}
