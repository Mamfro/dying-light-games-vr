//! What only Dying Light 1 knows: its engine facts, its hooks, and reading and writing the game's
//! state. The rule for this folder: a provider supplies something once for Monaka's ready-mades to
//! read, and does no feature's work itself (that would be cooking, waiting in a `cooking` folder).
//! Each file says which provider it is, or will be once Monaka has the trait.

pub(crate) mod actions;
pub(crate) mod bow;
pub(crate) mod climbing;
pub(crate) mod depth;
pub(crate) mod engine;
pub(crate) mod eye_site;
pub(crate) mod frame_labels;
pub(crate) mod fsr;
pub(crate) mod hand_world;
pub(crate) mod hud_draws;
pub(crate) mod hud_drawing;
pub(crate) mod hud_pieces;
pub(crate) mod hud_widgets;
pub(crate) mod lockpick;
pub(crate) mod look_fields;
pub(crate) mod melee;
pub(crate) mod present_entry;
pub(crate) mod projectile;
pub(crate) mod skeleton;
pub(crate) mod steps;
pub(crate) mod throws;
pub(crate) mod video;
