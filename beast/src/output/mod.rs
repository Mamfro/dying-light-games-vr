//! How finished images leave the game: the D3D12 present request and the hand-over to the shared
//! alternate-eye publisher (`present12`), FSR frame generation (`framegen`), per-eye DLSS with its
//! motion vectors rebased for each eye (`dlss`, `monaka_framegen::motion`), the Streamline tags they read
//! (`streamline`), and the switch to the headset's eye size (`video`).

pub(crate) mod dlss;
pub(crate) mod framegen;
pub(crate) mod present12;
pub(crate) mod streamline;
pub(crate) mod video;
