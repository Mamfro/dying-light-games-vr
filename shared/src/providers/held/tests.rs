use super::*;

const KINDS: [Kind; 4] = [Kind::Gun, Kind::Melee, Kind::Blade, Kind::Nothing];

#[test]
fn the_hands_hold_what_they_held_before_with_the_bow_in_the_left() {
    for aim in [LEFT_HAND, RIGHT_HAND] {
        for held in KINDS {
            let weapon = if held == Kind::Nothing { 0 } else { 0x1000 };
            for bow in [None, Some(0x2000)] {
                let noted = Noted::of(aim, weapon, held, bow, [Some(7), Some(9)], false);
                // The kinds kept before: the aim hand the weapon's, the other nothing.
                let before = [LEFT_HAND, RIGHT_HAND].map(|side| if side == aim { held } else { Kind::Nothing });
                for side in [LEFT_HAND, RIGHT_HAND] {
                    let now = noted.sides[side].kind;
                    let bowed = bow.is_some() && side == LEFT_HAND && before[side] == Kind::Nothing;
                    assert_eq!(now, if bowed { Kind::Bow } else { before[side] }, "{aim} {held:?} {bow:?} {side}");
                    // What the hand rig does with them is unchanged: a free hand's fingers, a gun's grip.
                    assert_eq!(now.grips(), before[side] != Kind::Nothing);
                    assert_eq!(now == Kind::Gun, before[side] == Kind::Gun);
                }
                assert_eq!(noted.grip, if bow.is_some() { Grip::Apart } else { Grip::Measured });
            }
        }
    }
}

/// The tests that note what the hands hold, one at a time: it is one for the run.
pub(super) static NOTING: Mutex<()> = Mutex::new(());

#[test]
fn an_item_is_its_visual_on_its_holder() {
    let _serial = NOTING.locked();
    note(Noted::of(RIGHT_HAND, 0x1000, Kind::Melee, None, [Some(7), Some(9)], true));
    assert_eq!(item(RIGHT_HAND), Some(Item { key: 0x1000, grip: Grip::Apart }));
    assert_eq!((holder_of(LEFT_HAND), holder_of(RIGHT_HAND)), (Some(7), Some(9)), "each hand's holder, held or not");
    assert_eq!(item(LEFT_HAND), None, "an empty hand has no item");
    assert_eq!((kind(RIGHT_HAND), kind(LEFT_HAND)), (Kind::Melee, Kind::Nothing));
    let dl1_gun = Line { from: [0.0; 3], along: [0.0, 0.0, 1.0], length: Some(0.25), edge: None };
    note(Noted::of(RIGHT_HAND, 0x1000, Kind::Gun, None, [None; 2], false));
    assert_eq!(line(RIGHT_HAND, &Lines { gun: Some(dl1_gun), melee: None }), Some(dl1_gun));
    assert_eq!(line(RIGHT_HAND, &FROM_MODELS), None, "a gun's line is the game's own or none");
    note(Noted::of(RIGHT_HAND, 0, Kind::Nothing, None, [None; 2], false));
}

#[test]
fn a_throwable_is_held_and_lined_as_the_melee_weapon_it_was_reported_as() {
    let _serial = NOTING.locked();
    let dl1 = Lines { gun: None, melee: Some(Line { from: [0.0; 3], along: [0.0, 1.0, 0.0], length: None, edge: Some([0.0, 0.0, 1.0]) }) };
    for held in [Kind::Gun, Kind::Melee, Kind::Blade, Kind::Nothing] {
        for (throwable, throwing) in [(false, false), (true, false), (false, true), (true, true)] {
            let now = reported(held, throwable, throwing);
            assert_eq!(now == Kind::Throwable, held == Kind::Melee && (throwable || throwing), "{held:?}");
            // What the hand rig does with it is unchanged: the grip, a gun's hold.
            assert_eq!((now.grips(), now == Kind::Gun), (held.grips(), held == Kind::Gun), "{held:?}");
            // The line it had before.
            let weapon = if held == Kind::Nothing { 0 } else { 0x1000 };
            note(Noted::of(RIGHT_HAND, weapon, held, None, [None; 2], false));
            let before = line(RIGHT_HAND, &dl1);
            note(Noted::of(RIGHT_HAND, weapon, now, None, [None; 2], false));
            assert_eq!(line(RIGHT_HAND, &dl1), before, "{held:?} {throwable} {throwing}");
        }
    }
    note(Noted::of(RIGHT_HAND, 0, Kind::Nothing, None, [None; 2], false));
}
