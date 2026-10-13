//! What only the Chrome Engine knows: its facts, its hooks, and reading and writing the game's
//! state, for the three games on it. The rule for this folder: a provider supplies something once
//! for Monaka's ready-mades to read, and does no feature's work itself (that would be cooking,
//! waiting in a `cooking` folder). Each file says which provider it is, or will be once Monaka has
//! the trait; a "provider part" is what the Chrome games share of a provider each game implements.

pub mod actions;
pub mod aim;
pub mod bow;
pub mod coskeleton;
pub mod fpp;
pub mod game;
pub mod grabs;
pub mod gui;
pub mod hands;
pub mod held;
pub mod hud_over;
pub mod hudlayer;
pub mod lockpick;
pub mod melee;
pub mod look_fields;
pub mod model;
pub mod rd3d12;
pub mod rig;
pub mod throws;
pub mod view_bob;
pub mod walk;
