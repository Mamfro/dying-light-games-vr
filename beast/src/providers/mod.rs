//! What only The Beast knows: its engine facts, its hooks, and reading and writing the game's
//! state. The rule for this folder: a provider supplies something once for Monaka's ready-mades to
//! read, and does no feature's work itself (that would be cooking; The Beast has none). Each file
//! says which provider it is, or will be once Monaka has the trait.

pub(crate) mod actions;
pub(crate) mod anti_wall;
pub(crate) mod camera_update;
pub(crate) mod cameras;
pub(crate) mod controllers;
pub(crate) mod dlss;
pub mod engine;
pub(crate) mod eye_site;
pub(crate) mod fsr;
pub(crate) mod hand_rig;
pub(crate) mod hud_pieces;
pub(crate) mod look_fields;
pub(crate) mod present12;
pub(crate) mod stereo;
pub(crate) mod video;
