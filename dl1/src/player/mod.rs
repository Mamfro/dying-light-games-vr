//! What the body does: head and hand aim with the mod-owned facing (`aim`), the arms on the
//! controllers over the shared Chrome rig (`hands`), physical melee (`melee`), the player's walk
//! body and room-scale following (`walk`, `roomscale`), zombie grabs (`grabs`), and named spots to come back to for testing
//! (`spots`).

pub(crate) mod aim;
pub(crate) mod grabs;
pub(crate) mod hands;
pub(crate) mod melee;
pub(crate) mod roomscale;
pub(crate) mod spots;
pub(crate) mod walk;
