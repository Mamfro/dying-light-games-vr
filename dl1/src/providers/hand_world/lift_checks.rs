//! DL1's hands as the hand rig's items against the per-feature items DL1 had: the throw read its
//! hands' elements (`HANDS`), the throw and the reload the controllers' trail (`CONTROLLERS`), the
//! bow the point between the right hand's fingers; DL1's rig had no trail of its own to note.

use super::*;
use crate::Dl1;
use monaka_arms::hand_rig::{self, HandRig};

#[test]
fn the_hands_are_the_ones_read_before() {
    HANDS.note_positions([Some([1.5, -2.25, 3.0]), None]);
    for side in [LEFT, RIGHT] {
        assert_eq!(hand_rig::posed_hand::<Dl1>(side), HANDS.position(side), "the throw's hand {side}");
    }
    assert!(std::ptr::eq(<Dl1 as HandRig>::trail().unwrap(), &CONTROLLERS), "the throw's and the reload's controllers");
    assert_eq!(<Dl1 as HandRig>::string_point(), string_point(), "the bow's string hand");
    const { assert!(!<Dl1 as HandRig>::NOTES_TRAIL) };
}
