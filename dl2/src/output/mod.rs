//! How finished images leave the game: the eye images taken from the back buffer at each
//! present request and published (`render11`, `render12`), the depth warp and the HUD lay-over
//! (`warp12`), FSR frame generation per eye (`framegen`), per-eye DLSS and its history (`dlss`,
//! `temporal`), the Streamline tags they read (`streamline`), and the switch to the headset's eye
//! size (`video`).

pub(crate) mod dlss;
pub(crate) mod framegen;
pub(crate) mod render11;
pub(crate) mod render12;
pub(crate) mod streamline;
pub(crate) mod temporal;
pub(crate) mod video;
pub(crate) mod warp12;
