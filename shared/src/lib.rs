//! Techland's Chrome Engine as Dying Light 2 and The Beast run it (and Dying Light 1 where noted):
//! head aim on the player character ([`headaim`]), the first-person arms on the controllers
//! ([`rig`] around the arms callback [`fpp`], their skeleton component [`coskeleton`], tracked
//! fingers on it [`fingers`]), the game object behind the video settings ([`game`]), the HUD as a
//! layer and its pieces ([`hudlayer`], [`gui`], [`panels`], [`hudfix`]), physical melee
//! ([`melee`]) and the player's movement step for room-scale following ([`walk`]), and the D3D12
//! renderer's present request and queue ([`rd3d12`]). Each game crate supplies the addresses of
//! its own build.

pub mod coskeleton;
pub mod fingers;
pub mod fpp;
pub mod game;
pub mod gui;
pub mod headaim;
pub mod hudfix;
pub mod hudlayer;
pub mod melee;
pub mod panels;
pub mod rd3d12;
pub mod rig;
pub mod walk;

/// The engine DLL (all three games).
pub const ENGINE: &str = "engine_x64_rwdi.dll";
/// The game DLL of Dying Light 2 and The Beast (Dying Light 1's has no `_ph`).
pub const GAMEDLL: &str = "gamedll_ph_x64_rwdi.dll";
/// The renderer DLLs, one per graphics API; a game loads the one its settings pick.
pub const RD3D11: &str = "rd3d11_x64_rwdi.dll";
pub const RD3D12: &str = "rd3d12_x64_rwdi.dll";
