//! Where each eye's camera goes: the alternate-eye schedule and the eye camera made from the head
//! pose (`stereo`), written at the renderer camera's exported setters (`cameras`) in the player camera
//! update (`camera_update`, with the PC check that a camera write reaches the picture).

pub(crate) mod camera_update;
pub(crate) mod cameras;
pub(crate) mod stereo;
