//! DL1's own versions of features Monaka should own. The rule for this folder: each file does a
//! feature's work itself, which belongs in a Monaka ready-made, and is here only because a provider
//! (or the ready-made) is still missing; each file's first line says which one it waits for. Once
//! that exists, the file goes, and DL1 lists the ready-made instead.

pub(crate) mod room_scale_rules;
