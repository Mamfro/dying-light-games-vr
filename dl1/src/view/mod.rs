//! Where each eye's camera goes: alternate-eye rendering in place, the eye camera written at the
//! view setup from the head pose, the live player camera turned to the view, and the present loop
//! on the shared driver (`stereo`).

pub(crate) mod stereo;
