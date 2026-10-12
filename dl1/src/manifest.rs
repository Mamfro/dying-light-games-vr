//! Dying Light's game info, for the launcher and the loader (`monaka_core::manifest`). Its options
//! are those of the components ([`crate::Dl1`], `monaka_producer::game::options`), then the
//! launcher's own for starting it through Steam.

use crate::Dl1;
use monaka_core::manifest::*;
use monaka_producer::game::{options, options_len};

const COMPONENTS: usize = options_len::<Dl1>("dl1");
const DL1: [Opt; COMPONENTS + 2] = {
    let components: [Opt; COMPONENTS] = options::<Dl1, COMPONENTS>("dl1");
    let mut all = [LAUNCH; COMPONENTS + 2];
    let mut i = 0;
    while i < COMPONENTS {
        all[i] = components[i];
        i += 1;
    }
    all[COMPONENTS + 1] = settle(120.0, 20.0);
    all
};

pub const MANIFESTS: &[Manifest] = &[Manifest {
    id: "dl1",
    name: "Dying Light",
    status: Status::Beta,
    executables: &["DyingLightGame.exe"],
    steam_app: 239140,
    producer: "monaka_dl1",
    log: "monaka_dl1.log",
    options_file: "dl1-options.txt",
    run_prefix: "dl1",
    launch: Launch::Auto { settle_seconds: 20 },
    refused_arguments: &[("/3dtv", "Remove /3dtv from the game's launch options in Steam; VR uses the normal renderer.")],
    before: "In the game's video options, turn off NVIDIA HBAO+, depth of field and PCSS shadows (in VR they make shadows slide).",
    options: &DL1,
    licences: &[],
    left_hand_aims: true,
    weapon_ring: true,
}];
