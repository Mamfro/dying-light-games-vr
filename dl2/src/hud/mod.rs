//! The HUD's world markers (objectives, waypoints) at their targets' distance. The HUD layer
//! itself and its pieces on the hands are `eng_chr`'s (`hudlayer`, `gui`, `panels`); its lay-over
//! over each eye is in `output::render12`.

pub(crate) mod markers;
