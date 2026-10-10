//! What the body does: head and hand aim with the mod-owned facing (`aim`), the arms on the
//! controllers over the shared Chrome rig (`hands`), physical melee (`melee`), the player's walk
//! body and room-scale following (`walk`, `roomscale`), zombie grabs (`grabs`), the VR button layout, crouching and the reload gesture (`controls`), the hands in the world
//! (`hand_world`) and what they do there: throwing tools (`throwing`), the bow (`bow`), guns
//! fired from the barrel (`gun`; both through `projectile`) and lockpicking (`lockpick`), and named spots to come back to for testing
//! (`spots`).

pub(crate) mod aim;
pub(crate) mod bow;
pub(crate) mod controls;
pub(crate) mod grabs;
pub(crate) mod gun;
pub(crate) mod hand_world;
pub(crate) mod hands;
pub(crate) mod lockpick;
pub(crate) mod projectile;
pub(crate) mod melee;
pub(crate) mod roomscale;
pub(crate) mod spots;
pub(crate) mod throwing;
pub(crate) mod walk;
