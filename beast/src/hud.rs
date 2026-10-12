//! The HUD as a layer laid over each eye, and its pieces on the hands (`eng_chr::feature`'s
//! ready-made components; the lay-over at each present is `stereo::present12`'s).

use crate::{Beast, stereo};
use monaka_producer::game::Component;

pub const HUD: Component<Beast> = Component { needs: &[&stereo::EYES], ..eng_chr::feature::hud() };
pub const PANELS: Component<Beast> = Component { needs: &[&HUD], ..eng_chr::feature::panels() };
