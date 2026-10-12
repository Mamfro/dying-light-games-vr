//! The hand rig and what rides on it: the arms on the controllers at the arms visual's callback
//! ([`hands`], `eng_chr::rig`), their fingers, physical melee, the hands' throw, bow and reload,
//! and room-scale following (`eng_chr::feature`'s ready-made components).

pub(crate) mod hands;

use crate::{Beast, look, research};
use monaka_producer::game::{Component, Missing, Start};

/// The arms on the controllers at the arms visual's callback (`eng_chr::rig`), a swing of a held
/// weapon or a fist attacking (`swing_attack`, `swing_speed`).
pub const ARMS: Component<Beast> = Component {
    needs: &[&look::LOOK],
    install: |start| hands::configure(&start.build.engine, hands_settings(start, true)).missing(),
    ..eng_chr::feature::arms()
};

/// The rig's options for this run, posing the arms (`rig`) or only probing them.
pub(crate) fn hands_settings(start: &Start<'_, Beast>, rig: bool) -> hands::Settings {
    hands::Settings::read(start.options, start.shared, rig, research::options().hands)
}

/// A free hand's fingers on the tracked fingers (`eng_chr::fingers`, at the arms' pose).
pub const FINGERS: Component<Beast> = Component { needs: &[&ARMS], ..monaka_arms::feature::fingers() };
pub const MELEE: Component<Beast> = Component { needs: &[&ARMS], ..eng_chr::feature::melee() };
pub const HANDWORK: Component<Beast> = Component { needs: &[&ARMS], ..eng_chr::feature::handwork() };
pub const ROOM_SCALE: Component<Beast> = Component { needs: &[&ARMS], ..eng_chr::feature::room_scale() };
