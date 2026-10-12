//! The hand rig and what rides on it: the arms on the controllers at the arms visual's callback
//! ([`hands`], `eng_chr::rig`), their fingers, physical melee, the hands' throw, bow and reload,
//! and room-scale following (`eng_chr::feature`'s ready-made components).

pub(crate) mod hands;

use crate::{Dl2, look, research};
use eng_chr::feature::gamedll;
use monaka_producer::game::{Component, Lack, Missing, Start};

/// The arms on the controllers at the arms visual's camera-target callback (`eng_chr::rig`), a
/// swing of a held weapon or a fist attacking (`swing_attack`, `swing_speed`).
pub const ARMS: Component<Dl2> = Component { needs: &[&look::LOOK], install: |start| install_hands(start, true), release: hands::report, ..eng_chr::feature::arms() };

/// The arms visual's callback hooked, posing the arms (`rig`) or only probing them.
pub(crate) fn install_hands(start: &mut Start<'_, Dl2>, rig: bool) -> Result<(), Lack> {
    let gamedll = gamedll(start)?;
    let options = eng_chr::rig::Options::read(start.options, start.shared, rig, research::options().hands);
    // SAFETY: the game DLL's hash was checked.
    unsafe { hands::install(start.hooks, &start.build.engine, gamedll, options) }.missing()
}

/// A free hand's fingers on the tracked fingers (`eng_chr::fingers`, at the arms' pose).
pub const FINGERS: Component<Dl2> = Component { needs: &[&ARMS], ..monaka_arms::feature::fingers() };
pub const MELEE: Component<Dl2> = Component { needs: &[&ARMS], ..eng_chr::feature::melee() };
pub const HANDWORK: Component<Dl2> = Component { needs: &[&ARMS], ..eng_chr::feature::handwork() };
pub const ROOM_SCALE: Component<Dl2> = Component { needs: &[&ARMS], ..eng_chr::feature::room_scale() };
