//! Techland's Chrome Engine as Dying Light 2 and The Beast run it (and Dying Light 1 where
//! noted), shared by the three game crates; each supplies the addresses of its own build.
//!
//! The code is sorted by who should own it: [`providers`] (what only the Chrome Engine knows: the
//! arms skeleton and the callback that poses it, the rig's glue, the palms in the world, what the
//! hands hold and the actions on the pad for each game's providers, the look fields, a held
//! weapon's model, grabs held off the player, the game object behind the video settings, the HUD as
//! a layer and its pieces, its place over each eye, the player's movement step, view bobbing, the
//! hooks behind melee hits, the throw, the bow, the reload and the lockpick, and the D3D12
//! renderer's present request and queue) and [`cooking`] (the engine's own versions of features a
//! Monaka ready-made should own, each waiting for a provider: head aim's steering and the dynamic
//! HUD), and [`research`] (the probes the games share).
//!
//! The features as ready-made components, for a game that implements [`feature::Chrome`] (the
//! Chrome facts its build has been read for): [`feature`]. It stays at the crate's root: it is the
//! engine's component list, the provider trait every Chrome game implements beside the components
//! it lists, and not one provider or one piece of cooking.

pub mod cooking;
pub mod feature;
pub mod providers;
pub mod research;

/// The engine DLL (all three games).
pub const ENGINE: &str = "engine_x64_rwdi.dll";
/// The game DLL of Dying Light 2 and The Beast (Dying Light 1's has no `_ph`).
pub const GAMEDLL: &str = "gamedll_ph_x64_rwdi.dll";
/// The renderer DLLs, one per graphics API; a game loads the one its settings pick.
pub const RD3D11: &str = "rd3d11_x64_rwdi.dll";
pub const RD3D12: &str = "rd3d12_x64_rwdi.dll";
