//! What only Dying Light 2 knows: its engine facts, its hooks, and reading and writing the game's
//! state. The rule for this folder: a provider supplies something once for Monaka's ready-mades to
//! read, and does no feature's work itself (that would be cooking, waiting in a `cooking` folder).
//! Each file says which provider it is, or will be once Monaka has the trait.

pub(crate) mod actions;
pub(crate) mod arms;
pub(crate) mod camera;
pub(crate) mod dlss;
pub(crate) mod engine;
pub(crate) mod eye_site;
pub(crate) mod game;
pub(crate) mod head;
pub(crate) mod hud;
pub(crate) mod look_fields;
pub(crate) mod present;
pub(crate) mod stereo;
pub(crate) mod streamline;
pub(crate) mod video;
