//! Where each eye's camera goes: the head pose, from the viewer or synthetic on PC runs (`head`),
//! the stereo pair on the engine side, each eye's camera and frustum written into the scene
//! (`scene`), and the eye camera maths in Dying Light 2's camera form (`camera`).

pub(crate) mod camera;
pub(crate) mod head;
pub(crate) mod scene;
