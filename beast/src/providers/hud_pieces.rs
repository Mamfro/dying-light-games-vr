//! Provider: the HUD pieces for Monaka's dynamic HUD (`monaka_stereo::hud_panels::LayerPieces`, its
//! layer crops): The Beast's pieces ([`engine::PIECES`]) and where each lies in the HUD layer this
//! frame, read from the engine's gui tree (`eng_chr::providers::gui`).

use crate::Beast;
use crate::providers::engine;
use monaka_stereo::hud_panels::{HudPieces, LayerPieces, Piece};

impl HudPieces for Beast {
    const PIECES: &'static [Piece] = &engine::PANELS;
    const MARGIN: f32 = eng_chr::providers::gui::MARGIN;
}

impl LayerPieces for Beast {
    fn boxes(boxes: &mut [Option<[f32; 4]>]) {
        eng_chr::providers::gui::boxes(boxes);
    }
}

#[cfg(test)]
mod tests;
