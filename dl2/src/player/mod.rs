//! What the body does: head and hand aim on the player character (`aim`) and the arms on the
//! controllers (`hands`), over `eng_chr`'s shared head aim and rig. Physical melee and room-scale
//! following are `eng_chr::melee` and `eng_chr::walk` with this build's facts (`engine::MELEE`,
//! `engine::WALK`).

pub(crate) mod aim;
pub(crate) mod hands;
