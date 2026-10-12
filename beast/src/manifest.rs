//! The Beast's game info, for the launcher and the loader (`monaka_core::manifest`). Its options are
//! those of the components ([`crate::Beast`], `monaka_producer::game::options`).

use crate::Beast;
use monaka_core::manifest::*;
use monaka_producer::game::{options, options_len};

const BEAST: [Opt; options_len::<Beast>("beast")] = options::<Beast, { options_len::<Beast>("beast") }>("beast");

pub const MANIFESTS: &[Manifest] = &[Manifest {
    id: "beast",
    name: "Dying Light: The Beast",
    status: Status::Experimental,
    executables: &["DyingLightGame_TheBeast_x64_rwdi.exe"],
    steam_app: 3008130,
    producer: "monaka_beast",
    log: "monaka_beast.log",
    options_file: "beast-options.txt",
    run_prefix: "beast",
    launch: Launch::Manual,
    refused_arguments: &[],
    before: "Use DirectX 12 with the game's own Frame Generation and motion blur off. Start the game and load into gameplay first.",
    options: &BEAST,
    licences: &[],
    left_hand_aims: true,
    weapon_ring: true,
    controls: &[],
}];
