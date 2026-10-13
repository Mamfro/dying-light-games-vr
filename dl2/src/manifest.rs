//! Dying Light 2's game info, for the launcher and the loader (`monaka_core::manifest`). Its options
//! are those of the components ([`crate::Dl2`], `monaka_producer::game::options`), then the
//! launcher's own for starting it through Steam.

use crate::Dl2;
use monaka_core::manifest::*;
use monaka_producer::game::{options, options_len};

const COMPONENTS: usize = options_len::<Dl2>("dl2");
const DL2: [Opt; COMPONENTS + 2] = {
    let components: [Opt; COMPONENTS] = options::<Dl2, COMPONENTS>("dl2");
    let mut all = [LAUNCH; COMPONENTS + 2];
    let mut i = 0;
    while i < COMPONENTS {
        all[i] = components[i];
        i += 1;
    }
    all[COMPONENTS + 1] = settle(180.0, 30.0);
    all
};

pub const MANIFESTS: &[Manifest] = &[Manifest {
    id: "dl2",
    name: "Dying Light 2",
    status: Status::PreAlpha,
    executables: &["DyingLightGame_x64_rwdi.exe"],
    steam_app: 534380,
    producer: "monaka_dl2",
    log: "monaka_dl2.log",
    options_file: "dl2-options.txt",
    run_prefix: "dl2",
    launch: Launch::Auto { settle_seconds: 30 },
    refused_arguments: &[],
    before: "In the game's options, use DirectX 12. The game's own Frame Generation can be on or off (Monaka VR holds it off while VR runs); for Monaka's frame generation, turn it OFF.",
    options: &DL2,
    licences: &["DyingLight2VR-LICENSE.txt"],
    left_hand_aims: true,
    weapon_ring: true,
    controls: &[],
}];
