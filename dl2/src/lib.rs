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
//! DLL copy (`config`, the manifest's options). The probes are in `research`.

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

use view::{head, scene};
use output::{dlss, framegen, render11, render12, streamline, video};
use hud::markers;
use player::{aim, hands};

use config::Config;
use monaka_channel::pose::PoseChannel;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight};
use monaka_producer::{Rejection, Session, enable_hooks, log, remove_hooks, require_build};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

monaka_producer::producer!(manifests: crate::manifest::MANIFESTS, start: start, stop: stop);

static HOOKS: Mutex<Option<Hooks>> = Mutex::new(None);

/// Presents of the game's window (D3D11) or present requests (D3D12).
pub static PRESENTS: AtomicU64 = AtomicU64::new(0);
static PUBLISHED: AtomicU64 = AtomicU64::new(0);
static SKIPPED: AtomicU64 = AtomicU64::new(0);

/// One publish attempt: counted, and every 600 presents the run's counters are logged.
pub fn count(ok: bool) {
    if ok { &PUBLISHED } else { &SKIPPED }.fetch_add(1, Ordering::Relaxed);
    let n = PRESENTS.load(Ordering::Relaxed);
    if n == 1 || n.is_multiple_of(600) {
        report();
    }
}

fn report() {
    log!(
        "presents={} published={} skipped={} stereo={} paused={}",
        PRESENTS.load(Ordering::Relaxed),
        PUBLISHED.load(Ordering::Relaxed),
        SKIPPED.load(Ordering::Relaxed),
        scene::STEREO_PAIRS.load(Ordering::Relaxed),
        scene::REJECTED_PAIRS.load(Ordering::Relaxed)
    );
    scene::report();
    let renderer = if game::try_get().is_some_and(|g| g.renderer11.is_some()) { render11::report() } else { render12::report() };
    log!(
        "DLSS eye calls={} resets={} matrix failures={}; {renderer}; {}; {}",
        dlss::EYE_CALLS.load(Ordering::Relaxed),
        dlss::RESETS.load(Ordering::Relaxed),
        dlss::MATRIX_FAILURES.load(Ordering::Relaxed),
        streamline::report(),
        framegen::report()
    );
    log!("{}", scene::timing_report());
    markers::report();
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

fn start(session: &Session) -> Result<(), Rejection> {
    let engine_module = require_build(engine::ENGINE, engine::ENGINE_SHA256)?;
    let (renderer11, renderer12) = (Module::find(engine::RENDERER_11), Module::find(engine::RENDERER_12));
    let renderer = match (&renderer11, &renderer12) {
        (Some(_), None) => require_build(engine::RENDERER_11, engine::RENDERER_11_SHA256)?,
        (None, Some(_)) => require_build(engine::RENDERER_12, engine::RENDERER_12_SHA256)?,
        _ => return Err(Rejection::not_ready("expected exactly one of rd3d11/rd3d12 loaded")),
    };
    let d3d11 = renderer11.is_some();
    let options = session.manifest_options(&manifest::MANIFESTS[0]);
    let mut config = Config::from_options(&options);
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
    research::configure(&options);
    let probes = research::options();
    monaka_producer::install_crash_log();
    let channel = session.require_channel()?;

    // The game DLL carries head aim and the pitch lock; a different build only loses those.
    let gamedll = Module::find(engine::GAMEDLL).filter(|m| match m.file_sha256() {
        Some(hash) if hash == engine::GAMEDLL_SHA256 => true,
        other => {
            log!("{} is build {other:?}, not the inspected one; head aim and the pitch lock are off", engine::GAMEDLL);
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

    // Head aim, hand aim and the arms on the controllers, on the inspected game DLL.
    aim::AIM.configure(config.arms_aim.only_if(gamedll.is_some()), gamedll.as_ref().map_or(0, Module::base));
    let mut hooks = Hooks::default();
    // SAFETY: every target is checked against its exact prologue (or is decoded, for system and
    // Streamline code); every detour has its target's type.
    unsafe {
        scene::install(&mut hooks, &engine_module)?;
        if config.aim.head
            && gamedll.is_some()
            && let Some(target) = engine_module.export(engine::FROM_FORWARD_EXPORT).filter(|&t| t == engine_module.at(engine::FROM_FORWARD_UP_POS.0))
        {
            hooks.inline(&aim::FROM_FORWARD, "FromForwardUpPos", target, engine::FROM_FORWARD_UP_POS.1, aim::from_forward as engine::FromForwardFn)?;
        } else if config.aim.head {
            log!("head aim unavailable (player camera setter not hooked)");
        }
        if let Some(gamedll) = &gamedll {
            match aim::pitch_actions_match(&engine_module) {
                Ok(()) => {
                    let target = gamedll.at(engine::INPUT_ACTION.0);
                    hooks.inline(&aim::INPUT_ACTION, "input action", target, engine::INPUT_ACTION.1, aim::input_action as engine::InputActionFn)?;
                }
                Err(why) => log!("vertical look not locked: {why}"),
            }
        }
        if config.controller_pad {
            monaka_pad::install_optional(&mut hooks, &channel);
        }
        // The HUD's world markers: their world points each tick, for the probe and for drawing
        // them at their targets' depth (every D3D12 mode with the dynamic HUD).
        let world_markers = config.hud.markers && config.hud.world && !d3d11;
        if (probes.markers || world_markers)
            && let Some(gamedll) = &gamedll
        {
            match markers::install(&mut hooks, gamedll) {
                Ok(()) => markers::enable(world_markers),
                Err(why) => log!("HUD markers off: {}", why.reason),
            }
        }
        // The engine's gui tree: for the dynamic HUD's pieces (depth stereo) and the probe.
        let pieces = config.hud.world && !d3d11;
        if probes.gui || pieces {
            match eng_chr::gui::install(&mut hooks, &engine_module, &engine::GUI_BUILD) {
                Ok(()) => eng_chr::gui::configure(probes.gui, if pieces { &engine::PIECES } else { &[] }, scene::publishing),
                Err(why) => log!("gui tree unavailable (no dynamic HUD): {}", why.reason),
            }
        }
        if (config.aim.rig || probes.hands)
            && let Some(gamedll) = &gamedll
            && let Err(why) = hands::install(&mut hooks, &engine_module, gamedll)
        {
            log!("hand rig off: {}", why.reason);
        }
        if let Some(physical) = config.physical_melee
            && let Some(gamedll) = &gamedll
            && let Err(why) = eng_chr::melee::install(&mut hooks, gamedll, engine::MELEE, physical)
        {
            log!("physical melee off: {}", why.reason);
        }
        if config.disable_zombie_grabs {
            eng_chr::grabs::enable();
        }
        if config.handwork != eng_chr::handwork::Options::default()
            && let Some(gamedll) = &gamedll
        {
            eng_chr::handwork::install(&mut hooks, gamedll, &engine::HANDWORK, config.handwork);
        }
        if let Some(settings) = config.room_scale
            && let Some(gamedll) = &gamedll
        {
            match eng_chr::walk::install(&mut hooks, &engine_module, gamedll, engine::WALK) {
                Ok(()) => monaka_arms::roomscale::install(settings),
                Err(why) => log!("room-scale following off: {}", why.reason),
            }
        }
        if (probes.melee || probes.sweep)
            && let Some(gamedll) = &gamedll
            && let Err(why) = research::melee::install(&mut hooks, gamedll, probes.melee, probes.sweep)
        {
            log!("melee probe off: {}", why.reason);
        }
        streamline::install(&mut hooks, &renderer);
        let at = |target: &engine::Target| renderer.at(target.0);
        if d3d11 {
            use engine::{COMMANDS_11, DLSS_CONSTANTS_11, DLSS_EVALUATE_11};
            hooks.inline(&render11::DLSS_EVALUATE, "DLSS evaluate", at(&DLSS_EVALUATE_11), DLSS_EVALUATE_11.1, render11::dlss_evaluate as engine::PacketFn)?;
            hooks.inline(&render11::DLSS_CONSTANTS, "DLSS constants", at(&DLSS_CONSTANTS_11), DLSS_CONSTANTS_11.1, render11::dlss_constants as engine::PacketFn)?;
            hooks.inline(&render11::COMMANDS, "command dispatcher", at(&COMMANDS_11), COMMANDS_11.1, render11::commands as engine::CommandsFn)?;
            render11::install_present(&mut hooks)?;
            render11::set_channel(channel.clone());
        } else {
            use engine::{DLSS_CONSTANTS_12, DLSS_EVALUATE_12, PRESENT_REQUEST_12};
            hooks
                .inline(&render12::PRESENT_REQUEST, "rd3d12 present request", at(&PRESENT_REQUEST_12), PRESENT_REQUEST_12.1, render12::present_request as engine::PresentRequest12Fn)
                ?;
            hooks.inline(&render12::DLSS_EVALUATE, "DLSS evaluate (D3D12)", at(&DLSS_EVALUATE_12), DLSS_EVALUATE_12.1, render12::dlss_evaluate as engine::Packet12Fn)?;
            hooks.inline(&render12::DLSS_CONSTANTS, "DLSS constants (D3D12)", at(&DLSS_CONSTANTS_12), DLSS_CONSTANTS_12.1, render12::dlss_constants as engine::Packet12Fn)?;
            render12::set_channel(channel.clone());
            // The HUD as a layer of its own from the game's command lists, kept out of the frame:
            // the warp lays it over the eyes and the pieces go to the hands; the game's own UI
            // tag (its frame generation on) is not needed. `probe_hud` probes it.
            if pieces || probes.hud {
                match render12::game_queue() {
                    // SAFETY: the game's live direct queue (within the block's own unsafe).
                    Some(queue) => {
                        if let Err(e) = eng_chr::hudlayer::install(&mut hooks, &queue, true, probes.hud, Some(channel.clone())) {
                            log!("hud layer off: {e}");
                        }
                    }
                    None => log!("hud layer off: the game queue is not known yet"),
                }
            }
        }
    }
    // A synthetic head still writes pose records (the viewer and the probe read them).
    head::open(PoseChannel::open(&channel).map_err(|e| log!("pose channel: {e}; publishing mono")).ok());
    if config.aim.hand || config.aim.rig {
        // The hand block, and the finger block beside it (finger tracking).
        monaka_arms::controllers::open(&channel);
        monaka_arms::fingers::set_binary(config.fingers.binary);
    }
    enable_hooks(&HOOKS, hooks)?;
    scene::PUBLISHING.store(true, Ordering::Release);
    let mode = if config.framegen && config.hud.world {
        "same-frame stereo with FSR frame generation per eye and the dynamic HUD"
    } else if config.framegen {
        "same-frame stereo with FSR frame generation per eye"
    } else if config.depth_stereo && config.hud.world {
        "depth stereo (one render, both eyes from depth) with the dynamic HUD"
    } else if config.depth_stereo {
        "depth stereo (one render, both eyes from depth)"
    } else if config.alternate_eye && config.hud.world {
        "alternate-eye stereo with the dynamic HUD"
    } else if config.alternate_eye {
        "alternate-eye stereo"
    } else if config.hud.world && !d3d11 {
        "same-frame stereo with the dynamic HUD"
    } else {
        "same-frame stereo"
    };
    let pose = if config.synthetic.is_some() {
        "a synthetic pose"
    } else if head::HEAD.connected() {
        "the viewer"
    } else {
        "nowhere (mono)"
    };
    log!("DL2 producer running on {}: {mode}; head pose from {pose}", if d3d11 { "D3D11" } else { "D3D12" });
    Ok(())
}

fn stop() -> Result<(), Rejection> {
    // Stop starting new pairs; a pair in progress restores the camera itself before returning.
    scene::PUBLISHING.store(false, Ordering::Release);
    monaka_pad::stop();
    scene::stop_stereo();
    video::restore();
    InFlight::wait_idle(2000);
    // The next player camera update hands the head's share of the look back to the mouse.
    aim::release(500);
    scene::abandon_pair();
    // Detours still running keep the channel open.
    eng_chr::hudlayer::remove(1000);
    remove_hooks(&HOOKS, 2000)?;
    framegen::close();
    render11::close();
    render12::close();
    head::HEAD.close();
    monaka_arms::controllers::close();
    hands::report();
    eng_chr::handwork::report();
    eng_chr::grabs::report();
    eng_chr::gui::report();
    monaka_arms::roomscale::stop();
    eng_chr::walk::report();
    research::report();
    monaka_pad::report();
    monaka_pad::release();
    report();
    monaka_producer::remove_crash_log();
    Ok(())
}
