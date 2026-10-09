//! The HUD and menus: placed in front of each eye at the back-buffer draws (`draws`), its pieces cut
//! onto the hands' panels by draw (`panels`), matched to the widgets of the game's UI tree (`ui`).

pub(crate) mod draws;
pub(crate) mod panels;
pub(crate) mod ui;
