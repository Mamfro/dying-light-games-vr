//! Stereo by alternate eyes on D3D12, on Monaka's present loop: the renderer camera's setters where
//! the player camera update is taken over ([`cameras`], [`camera_update`]), the present request
//! every frame goes through ([`present12`]), the eye cameras, labels and pose report ([`eyes`]),
//! the switch to the headset's eye size ([`video`]) and per-eye frame generation ([`framegen`]).

pub(crate) mod camera_update;
pub(crate) mod cameras;
pub(crate) mod eyes;
pub(crate) mod framegen;
pub(crate) mod present12;
pub(crate) mod video;

use crate::{Beast, Facts, engine};
use eng_chr::feature::gamedll;
use monaka_core::feature::Feature;
use monaka_core::manifest::{EYE_SEPARATION, Kind, LATENCY, Opt, Shown, choice, line, opt, render_size};
use monaka_core::options::Options;
use monaka_producer::game::{Component, Lack, On, OrMissing, Start, Switch};
use monaka_producer::log;
use monaka_stereo::feature::{Entry, Graphics, Loop, Presents};

/// `turn_yaw=<degrees>`: the player camera turned, the PC check that writing it reaches the
/// picture ([`camera_update`]).
pub(crate) fn turn_yaw(options: &Options) -> Option<f32> {
    options.angle("turn_yaw", 90.0).filter(|&yaw| yaw != 0.0)
}

/// The renderer camera's exported setters: the player camera update passes `SetView(vec3)`, where
/// the eye camera, head aim and the turn check take over ([`cameras`]), and
/// `ComputeFrustumMatrix`, where the headset's field of view goes in. Always on: the probes ride on
/// them too.
pub const CAMERAS: Component<Beast> = Component { name: "cameras", install: install_cameras, quiet: camera_update::disable, ..Component::NONE };

fn install_cameras(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let Facts { renderer, gamedll, .. } = start.build;
    if let Some(gamedll) = gamedll {
        camera_update::set_player_update(gamedll.at(engine::PLAYER_CAMERA_UPDATE));
    }
    start.hooks.site(&cameras::SET_VECTORS_ORIGINAL, renderer, &engine::SET_VIEW_VECTORS, cameras::set_vectors)?;
    start.hooks.site(&cameras::COMPUTE_FRUSTUM_ORIGINAL, renderer, &engine::COMPUTE_FRUSTUM, cameras::compute_frustum_detour())?;
    if let Some(yaw) = turn_yaw(start.options) {
        camera_update::enable_turn(yaw);
    }
    Ok(())
}

/// The D3D12 present request: the HUD laid over each frame, the present made inside the present
/// loop ([`present12`]).
pub const PRESENT_REQUEST: Component<Beast> = Component { name: "present request", install: install_present, ..Component::NONE };

fn install_present(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let r12 = start.build.renderer_12.as_ref().or_missing("the game is not on D3D12")?;
    present12::set_renderer(r12.base());
    start.hooks.site(&present12::PRESENT_REQUEST, r12, &engine::PRESENT_REQUEST_12, present12::present_request)?;
    Ok(())
}

/// Monaka's present loop, entered from the present request; it starts with the eyes, once the eye
/// size is asked for ([`eyes`]).
pub const PRESENT: Component<Beast> = Component {
    needs: &[&PRESENT_REQUEST],
    switch: Some(Switch::new("stereo", true, "stereo is off: the probes only")),
    arm: || {},
    ..monaka_stereo::feature::present()
};

/// The loop's steps for The Beast: frames labelled by their camera as DLSS gets it, the camera
/// updates taking the eye choices in turn, a menu flat, publishing held for the eye size, frame
/// generation's own pairs ([`eyes`]).
impl Presents for Beast {
    const LOOP: Loop<Self> = Loop {
        entry: Entry::Game,
        queue: Some(|_| present12::game_queue()),
        report: Some(eyes::report),
        before: Some(eyes::before_present),
        after: Some(eyes::after_present),
        adjust_pose: Some(eyes::latency_test),
        menu: Some(eng_chr::gui::menu_open),
        in_turn: true,
        pose_each_frame: Some(framegen::enabled),
        ..Loop::new(|_| Graphics::D3D12, ("eye cameras", &eyes::EYE_WRITES))
    };
}

const EYE_SIZE_OF_YOUR_OWN: Opt = line("eye_size", "Render size (WxH)", "A render size of your own; replaces render_size.", Kind::Size).launcher();

/// The game renders at the headset's eye size, windowed, while VR runs ([`video`]): asked for once
/// the hooks are in, publishing held until the game presents at it (the channel takes the first
/// published frame's size), so it starts before the eyes.
pub const EYE_SIZE: Component<Beast> = Component {
    name: "eye size",
    options: &[
        render_size("The headset's size runs much better.").eye_size_note("Click into the game: it switches to {size} once it has focus, and VR shows it from then on."),
        EYE_SIZE_OF_YOUR_OWN,
    ],
    needs: &[&PRESENT],
    on: |run| On::when(run.options.size("eye_size").is_some(), "the game renders at its own size"),
    install: install_eye_size,
    arm: video::switch,
    give_back: video::restore,
    ..Component::NONE
};

fn install_eye_size(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let eye = start.options.size("eye_size").or_missing("no eye size")?;
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

/// Stereo by alternate eyes ([`eyes`]): after each present the next eye and the head pose are
/// chosen, the player camera update sets the camera to that eye, and the frame is labelled by its
/// camera as DLSS gets it; `latency` presents later it goes out with its partner eye. `eye_test=`
/// and `latency_test=` (degrees) turn the eyes or every other pair's head for PC checks.
pub const EYES: Component<Beast> = Component {
    name: "eyes",
    features: &[Feature::Stereo],
    options: &[MODE, STEREO, LATENCY, EYE_SEPARATION],
    needs: &[&PRESENT],
    needs_channel: true,
    settings: |options, _| {
        framegen_on(options);
    },
    install: install_eyes,
    arm: eyes::arm,
    quiet: || eyes::quiet(1000),
    release: release_eyes,
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
    eyes::install(eyes::Settings {
        channel: channel.clone(),
        separation: shared.eye_separation,
        eye_test_degrees: options.angle("eye_test", 30.0).unwrap_or(0.0),
        latency_test_degrees: options.angle("latency_test", 30.0).unwrap_or(0.0),
        headset_fov: shared.headset_fov,
        hud: shared.hud.shown.then_some((shared.hud.scale, shared.hud.distance)),
    });
    Ok(())
}

fn release_eyes() {
    framegen::close();
    eyes::release();
}
