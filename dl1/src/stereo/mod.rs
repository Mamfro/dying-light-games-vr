//! Stereo by alternate eyes in place, on Monaka's present loop: the eye camera written at the view
//! setup from the head pose, the live player camera turned to the view, and DL1's steps around the
//! loop ([`eyes`]). How finished images leave the game beyond the plain alternate-eye publisher:
//! alternate eye with depth, the other eye reprojected ([`hybrid`]), FSR upscaling per eye
//! ([`upscale`]), frames with no 3D view to the flat screen ([`flat`]), and the switch to the
//! headset's eye size ([`video`]).

pub(crate) mod eyes;
pub(crate) mod flat;
pub(crate) mod hybrid;
pub(crate) mod upscale;
pub(crate) mod video;

use crate::config::Config;
use crate::engine::{self, CameraWriter, SetInverseFn};
use crate::hud::draws;
use crate::{Dl1, game_swapchain, research, stereo_on, swapchain_parts};
use monaka_channel::hands::HandChannel;
use monaka_core::feature::Feature;
use monaka_core::manifest::{EYE_SEPARATION, Kind, Opt, Shown, SteamFiles, choice, opt, render_size};
use monaka_core::situation::Situation;
use monaka_producer::game::{Component, Lack, Missing, On, OrMissing, Start, Switch};
use monaka_producer::{Rejection, log};
use monaka_stereo::feature::{Graphics, Loop, Presents};

/// The game renders at the headset's eye size while VR runs ([`video`]), switched before anything
/// takes the back buffer's size; with FSR, smaller, and the upscaler makes the eye size.
pub const EYE_SIZE: Component<Dl1> = Component {
    name: "eye size",
    options: &[render_size("If the game crashes, reset its resolution.")],
    on: |run| On::when(!research::options().flat && run.options.size("eye_size").is_some(), "the game renders at its own size"),
    install: install_eye_size,
    // The game applies it on its next frame; its levels then lay their menus out again.
    give_back: video::restore_and_relayout,
    ..Component::NONE
};

fn install_eye_size(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = crate::config(start);
    let Some(size) = config.eye_size else { return Ok(()) };
    let render = config.upscaled().map_or(size, |fsr| fsr.render_size(size));
    if let Err(why) = video::switch_to(&start.build.gamedll, &game_swapchain(&start.build.renderer)?, render, std::time::Duration::from_secs(8)) {
        log!("rendering at the game's own size: {why}");
    }
    Ok(())
}

/// Each frame to the viewer by Monaka's present loop, with DL1's steps ([`Dl1::LOOP`]).
pub const PRESENT: Component<Dl1> = Component { on: stereo_on, ..monaka_stereo::feature::present() };

/// DL1's own steps around Monaka's present loop ([`eyes`]): the upscaler, the hybrid and a flat
/// screen publish their own pairs; the HUD's draws end each frame; the end gives back what the run
/// changed.
impl Presents for Dl1 {
    const LOOP: Loop<Self> = Loop {
        frame_end: Some(draws::frame_end),
        report: Some(eyes::report),
        before: Some(eyes::before_present),
        after: Some(eyes::after_present),
        finish: Some(eyes::finish),
        ..Loop::new(|_| Graphics::D3D11, ("camera shifts", &eyes::SHIFTS))
    };
}

/// Stereo by alternate eyes, in place ([`eyes`]): the view setup moves the cached render camera to
/// the eye the present loop chose, the game asked for a field of view wide enough for both eyes;
/// with pointing aim, the controllers' block is read at each present.
pub const EYES: Component<Dl1> = Component {
    name: "eyes",
    features: &[Feature::Stereo],
    reports: &[Situation::WorldHidden],
    options: &[EYE_SEPARATION],
    needs: &[&PRESENT],
    needs_channel: true,
    settings: |options, manifest| {
        Config::from_options(options, manifest);
    },
    install: install_eyes,
    release: eyes::release,
    ..Component::NONE
};

fn install_eyes(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = crate::config(start);
    let engine_module = &start.build.engine;
    let set_inverse = engine_module.export(engine::SET_INVERSE_CAMERA).ok_or_else(|| Rejection::revision("the engine does not export SetInvCameraMatrix"))?;
    if let Some(missing) = engine::MESH_CULL_SETTERS.iter().find(|name| engine_module.export(name).is_none()) {
        return Err(Rejection::revision(format!("the engine does not export {missing}")).into());
    }
    let channel = start.channel()?;
    // Hand aim reads the controllers the viewer writes beside the pose; without them, head aim.
    let hands = if config.aim.hand { HandChannel::open(channel).map_err(|e| log!("hand aim off: hand block unavailable: {e}")).ok() } else { None };
    // SAFETY: the export is `void IBaseCamera::SetInvCameraMatrix(const mtx34&)` on x64.
    let camera = CameraWriter(unsafe { std::mem::transmute::<usize, SetInverseFn>(set_inverse) });
    eyes::install(eyes::Setup { config, camera }, hands);
    start.hooks.site(&eyes::VIEW_SETUP_ORIGINAL, engine_module, &engine::VIEW_SETUP, eyes::view_setup)?;
    // The field-of-view widening serves both the live-camera turn and head aim.
    if config.headset_fov && config.widen_game_fov {
        start.hooks.site(&eyes::SET_FOV_ORIGINAL, engine_module, &engine::SET_FOV, eyes::set_fov)?;
    }
    Ok(())
}

const FSR: Opt = opt("fsr", "FSR upscaling", "Renders each eye smaller, then upscales it (needs the headset resolution and Dying Light 2's FidelityFX DLLs).", Kind::Switch { default: false })
    .on(Shown::Engine)
    // Upscales to the eye size, with the FidelityFX runtime Dying Light 2 installs (Monaka ships none).
    .needs_eye_size()
    .needs_steam_files(SteamFiles {
        game_folder: "Dying Light 2",
        path: r"ph\work\bin\x64",
        file: "amd_fidelityfx_upscaler_dx12.dll",
        line: "fsr_dlls",
        missing: "FSR is off: it needs AMD's FidelityFX DLLs, which come with Dying Light 2 (not installed).",
    });
const FSR_SCALE: Opt = opt("fsr_scale", "FSR quality", "How much smaller each eye renders.", choice(&[("1.5", "Quality"), ("1.7", "Balanced"), ("2", "Performance"), ("3", "Ultra performance")], "1.5"))
    .on(Shown::Engine)
    .only_when("fsr", "1");
const FSR_SHARPNESS: Opt =
    opt("fsr_sharpness", "FSR sharpness", "0 turns sharpening off.", Kind::Number { min: 0.0, max: 1.0, step: 0.05, default: 0.3 }).on(Shown::Engine).only_when("fsr", "1");

/// FSR upscaling ([`upscale`]): each eye rendered smaller, AMD's FSR bringing it to the eye size,
/// with AMD's DLLs from `fsr_dlls=<folder>` or beside this DLL's copy.
pub const UPSCALE: Component<Dl1> = Component {
    name: "fsr",
    features: &[Feature::PerEyeHistory],
    options: &[FSR, FSR_SCALE, FSR_SHARPNESS],
    needs: &[&EYES],
    on: |run| On::when(Config::from_options(run.options, run.manifest).fsr.is_some(), "FSR is off"),
    install: install_upscale,
    ..Component::NONE
};

fn install_upscale(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = crate::config(start);
    let (fsr, display) = config.upscaled().zip(config.eye_size).or_missing("FSR needs the headset's eye size and does not combine with hybrid")?;
    let dlls = start.options.text("fsr_dlls").map_or_else(|| start.session.dir().to_path_buf(), std::path::PathBuf::from);
    let (_, device, _, render) = swapchain_parts(&start.build.renderer)?;
    log!("{}", upscale::install(&device, fsr, render, display, &dlls).missing()?);
    Ok(())
}

/// Experiment (`hybrid=1`): alternate-eye rendering with depth, a pair every frame, the other eye
/// reprojected from the real one ([`hybrid`]); the HUD a layer over both eyes.
pub const HYBRID: Component<Dl1> = Component {
    name: "hybrid",
    needs: &[&EYES],
    switch: Some(Switch::new("hybrid", false, "alternate eyes without depth")),
    install: install_hybrid,
    ..Component::NONE
};

fn install_hybrid(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let hud = start.shared.hud.shown;
    let (_, device, _, _) = swapchain_parts(&start.build.renderer)?;
    hybrid::install(&device, start.channel()?.clone(), hud).missing()?;
    log!("alternate-eye with depth: a pair every frame; HUD layer {hud}");
    Ok(())
}
