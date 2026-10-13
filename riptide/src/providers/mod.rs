//! What only Dead Island Riptide knows: its engine facts, its hooks, and reading and writing the
//! game's state. The rule for this folder: a provider supplies something once for Monaka's
//! ready-mades to read, and does no feature's work itself. Each file says which provider it is.

pub(crate) mod depth;
pub(crate) mod engine;
pub(crate) mod eye_site;
pub(crate) mod frame_labels;
pub(crate) mod hud_draws;
pub(crate) mod look_fields;
pub(crate) mod present_entry;
