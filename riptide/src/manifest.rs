//! Dead Island Riptide's game info, for the launcher and the loader (`monaka_core::manifest`). Its
//! options are what the player holds, then those of the components ([`crate::Riptide`],
//! `monaka_producer::game::options`), then the launcher's own for starting it through Steam; its
//! how-tos ([`CONTROLS`]) say how each thing is done in VR.

use crate::Riptide;
use monaka_core::manifest::*;
use monaka_producer::game::{options, options_len};

const COMPONENTS: usize = options_len::<Riptide>("riptide");
const RIPTIDE: [Opt; COMPONENTS + 3] = {
    let components: [Opt; COMPONENTS] = options::<Riptide, COMPONENTS>("riptide");
    let mut all = [LAUNCH; COMPONENTS + 3];
    all[0] = PLAY_WITH;
    let mut i = 0;
    while i < COMPONENTS {
        all[i + 1] = components[i];
        i += 1;
    }
    all[COMPONENTS + 2] = settle(120.0, 20.0);
    all
};

/// How each thing is done in Dead Island Riptide in VR, for the Controls tab.
const CONTROLS: &[HowTo] = &[HOW_BUTTONS];

pub const MANIFESTS: &[Manifest] = &[Manifest {
    id: "riptide",
    name: "Dead Island Riptide Definitive Edition",
    status: Status::Experimental,
    executables: &["DeadIslandRiptideGame.exe"],
    steam_app: 383180,
    producer: "monaka_riptide",
    log: "monaka_riptide.log",
    options_file: "riptide-options.txt",
    run_prefix: "riptide",
    launch: Launch::Auto { settle_seconds: 20 },
    before: "In the game's video options, turn Fullscreen off (in fullscreen the game changes size whenever it loses focus, and the headset image stops), and motion blur and chromatic aberration off.",
    options: &RIPTIDE,
    controls: CONTROLS,
    ..Manifest::GAME
}];
