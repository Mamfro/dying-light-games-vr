//! Provider part: the held item (`monaka_arms::held::HeldItems`), what the Chrome games share.
//!
//! What the arms hold in the Chrome Engine games, for each game's held-item provider
//! (`monaka_arms::held::HeldItems`, implemented by each game on these): noted at each arms callback
//! ([`note`], from [`crate::providers::fpp::around`]) from the arms visual's weapon visuals, and
//! read back by side. The aim hand holds the weapon visual's kind
//! ([`crate::providers::fpp::held_of`]), a throwable where the game says it is one ([`reported`]);
//! a bow is in the left hand, its string drawn by the right. Each item is its visual (a model of
//! its own, [`model`]), hanging from the hand's holder (`l_handholder`, `r_handholder`; [`holder`],
//! a game's `HandRig::holder` by [`holder_of`]); its line ([`line`]) is the game's own measured one
//! where it has one ([`Lines`]), else read from the model: a melee weapon's or a throwable's blade
//! toward its `DTrailStart1` ([`blade_line`]), a bow's arrow from its
//! nock (`monaka_arms::bow::nock_line`).

use crate::providers::model::Model;
use monaka_arms::Skeleton;
use monaka_arms::bow;
use monaka_arms::held::{Grip, Item, Line};
use monaka_arms::melee::Held;
use monaka_core::math::unit;
use monaka_core::protocol::{LEFT_HAND, RIGHT_HAND};
use monaka_core::sync::Locked;
use monaka_hook::probe::class_name;
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// One hand at the latest arms callback: the kind it holds, the visual (0 for none) and the
/// element it hangs from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Side {
    pub kind: Held,
    pub visual: usize,
    pub holder: Option<i32>,
}

const EMPTY: Side = Side { kind: Held::Nothing, visual: 0, holder: None };

/// Both hands at the latest arms callback, and how they hold what they hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Noted {
    pub sides: [Side; 2],
    pub grip: Grip,
}

impl Noted {
    /// The arms holding `weapon` (its visual, 0 for none) of kind `held` in the aim hand (`aim_side`)
    /// and the bow `bow` (its visual) if any, from the holders `holders`; each hand on its own
    /// controller when `apart` (a throw under way) or a bow is held. The bow is the left hand's
    /// where that hand holds nothing else.
    pub fn of(aim_side: usize, weapon: usize, held: Held, bow: Option<usize>, holders: [Option<i32>; 2], apart: bool) -> Self {
        let aim = aim_side.min(1);
        let mut sides = [LEFT_HAND, RIGHT_HAND].map(|side| {
            let (kind, visual) = if side == aim { (held, weapon) } else { (Held::Nothing, 0) };
            Side { kind, visual, holder: holders[side] }
        });
        if let Some(bow) = bow
            && sides[LEFT_HAND].kind == Held::Nothing
        {
            sides[LEFT_HAND] = Side { kind: Held::Bow, visual: bow, ..sides[LEFT_HAND] };
        }
        Self { sides, grip: if apart || bow.is_some() { Grip::Apart } else { Grip::Measured } }
    }
}

/// What the aim hand holding a weapon visual the melee takes as `held` ([`crate::providers::fpp::held_of`])
/// holds as the held item says: a throwable where the visual is one's (`throwable`) or the hand's
/// weapon slot holds what a throw under way throws (`throwing`), else as the melee takes it. A
/// throwable grips as a melee weapon does, so the hand rig holds it alike.
pub fn reported(held: Held, throwable: bool, throwing: bool) -> Held {
    match held {
        Held::Melee if throwable || throwing => Held::Throwable,
        held => held,
    }
}

static NOTED: Mutex<Noted> = Mutex::new(Noted { sides: [EMPTY; 2], grip: Grip::Measured });

/// What the arms hold as of this arms callback.
pub fn note(noted: Noted) {
    *NOTED.locked() = noted;
}

fn side(side: usize) -> Side {
    NOTED.locked().sides[side.min(1)]
}

/// The kind hand `side` holds as of the latest arms callback (a game's `HandRig::held`).
pub fn kind(side: usize) -> Held {
    self::side(side).kind
}

/// The item hand `side` holds (a game's `HeldItems::item`): its visual is its key.
pub fn item(side: usize) -> Option<Item> {
    let noted = *NOTED.locked();
    let hand = noted.sides[side.min(1)];
    (hand.visual != 0).then_some(Item { key: hand.visual as u64, grip: noted.grip })
}

/// The element hand `side` holds things by as of the latest arms callback (a game's
/// `HandRig::holder`).
pub fn holder_of(side: usize) -> Option<i32> {
    self::side(side).holder
}

/// The visual's model of what hand `side` holds (a game's `HeldItems::model`), on the game thread.
pub fn model(side: usize) -> Option<Model> {
    Model::of(self::side(side).visual)
}

/// A game's own lines for what its hands hold, each in the hand's holder frame (none: read from the
/// held weapon's model).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lines {
    pub gun: Option<Line>,
    pub melee: Option<Line>,
}

/// Every line read from the models (a gun has none).
pub const FROM_MODELS: Lines = Lines { gun: None, melee: None };

/// The line of what hand `side` holds (a game's `HeldItems::line`), on the game thread: `lines`'
/// where the game has one, else the model's (a throwable's as a melee weapon's: a knife's blade).
pub fn line(side: usize, lines: &Lines) -> Option<Line> {
    let hand = self::side(side);
    match hand.kind {
        Held::Gun => lines.gun,
        Held::Melee | Held::Blade | Held::Throwable => lines.melee.or_else(|| blade_line(hand.visual)),
        Held::Bow => bow_line(hand.visual),
        Held::Nothing | Held::Tool => None,
    }
}

/// The weapon model's point its blade sweeps toward.
const BLADE_TIP: &str = "dtrailstart1";
static NO_TIP: AtomicUsize = AtomicUsize::new(0);
static BLADE_LOGGED: AtomicUsize = AtomicUsize::new(0);

/// The melee weapon `weapon`'s (its visual's) blade in its model's frame, which sits on the hand's
/// holder: from the root toward `DTrailStart1` (the point the game's hit test sweeps toward), unit,
/// its length the game's to say.
pub fn blade_line(weapon: usize) -> Option<Line> {
    let model = Model::of(weapon)?;
    let Some(tip) = (0..model.count()).find(|&e| model.name(e).eq_ignore_ascii_case(BLADE_TIP)) else {
        if NO_TIP.swap(weapon, Relaxed) != weapon {
            log!("physical melee: the held weapon {} has no {BLADE_TIP}: its swings attack as before", class_name(weapon).unwrap_or_default());
        }
        return None;
    };
    let (root, at) = (model.world(0)?, model.world(tip)?);
    let axes = [0, 1, 2].map(|c| unit([root[c], root[4 + c], root[8 + c]]));
    let [Some(x), Some(y), Some(z)] = axes else { return None };
    let offset = [at[3] - root[3], at[7] - root[7], at[11] - root[11]];
    let along = unit([x, y, z].map(|a| a[0] * offset[0] + a[1] * offset[1] + a[2] * offset[2]))?;
    if BLADE_LOGGED.swap(weapon, Relaxed) != weapon {
        log!("physical melee: {}'s blade runs {along:.3?} in its model (from the root to {BLADE_TIP})", class_name(weapon).unwrap_or_default());
    }
    Some(Line { from: [0.0; 3], along, length: None, edge: None })
}

/// The bow model last read and its nock with the arrow's axis along it (none for a model without
/// a nock behind its bow).
type BowNock = (usize, Option<(i32, usize, f32)>);
static BOW_NOCK: Mutex<Option<BowNock>> = Mutex::new(None);

/// The arrow's line of the bow `bow` (its visual) against its root: from its nock as it is now.
fn bow_line(bow: usize) -> Option<Line> {
    let model = Model::of(bow)?;
    let found = {
        let mut known = BOW_NOCK.locked();
        match *known {
            Some((id, found)) if id == model.id() => found,
            _ => {
                let found = crate::providers::bow::BOWING.nock_in(&model).and_then(|nock| bow::nock_axis(&model, nock).map(|(axis, sign)| (nock, axis, sign)));
                *known = Some((model.id(), found));
                found
            }
        }
    };
    let (nock, axis, sign) = found?;
    Some(bow::nock_line(&model.world(nock)?, &model.world(0)?, axis, sign))
}

/// The holder the left or right hand (`side`) holds things by in `skeleton` (`l_handholder`,
/// `r_handholder`, by name without case: Dying Light 2 spells them `L_HandHolder`), found once per
/// skeleton.
pub fn holder(skeleton: &impl Skeleton, side: usize) -> Option<i32> {
    static FOUND: Mutex<Option<(usize, [Option<i32>; 2])>> = Mutex::new(None);
    let mut found = FOUND.locked();
    if let Some((id, holders)) = *found
        && id == skeleton.id()
    {
        return holders[side.min(1)];
    }
    let named = |name: &str| (0..skeleton.count()).find(|&e| skeleton.name(e).eq_ignore_ascii_case(name));
    let holders = [named("l_handholder"), named("r_handholder")];
    *found = Some((skeleton.id(), holders));
    holders[side.min(1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [Held; 4] = [Held::Gun, Held::Melee, Held::Blade, Held::Nothing];

    #[test]
    fn the_hands_hold_what_they_held_before_with_the_bow_in_the_left() {
        for aim in [LEFT_HAND, RIGHT_HAND] {
            for held in KINDS {
                let weapon = if held == Held::Nothing { 0 } else { 0x1000 };
                for bow in [None, Some(0x2000)] {
                    let noted = Noted::of(aim, weapon, held, bow, [Some(7), Some(9)], false);
                    // The kinds kept before: the aim hand the weapon's, the other nothing.
                    let before = [LEFT_HAND, RIGHT_HAND].map(|side| if side == aim { held } else { Held::Nothing });
                    for side in [LEFT_HAND, RIGHT_HAND] {
                        let now = noted.sides[side].kind;
                        let bowed = bow.is_some() && side == LEFT_HAND && before[side] == Held::Nothing;
                        assert_eq!(now, if bowed { Held::Bow } else { before[side] }, "{aim} {held:?} {bow:?} {side}");
                        // What the hand rig does with them is unchanged: a free hand's fingers, a gun's grip.
                        assert_eq!(now.grips(), before[side] != Held::Nothing);
                        assert_eq!(now == Held::Gun, before[side] == Held::Gun);
                    }
                    assert_eq!(noted.grip, if bow.is_some() { Grip::Apart } else { Grip::Measured });
                }
            }
        }
    }

    /// The tests that note what the hands hold, one at a time: it is one for the run.
    static NOTING: Mutex<()> = Mutex::new(());

    #[test]
    fn an_item_is_its_visual_on_its_holder() {
        let _serial = NOTING.locked();
        note(Noted::of(RIGHT_HAND, 0x1000, Held::Melee, None, [Some(7), Some(9)], true));
        assert_eq!(item(RIGHT_HAND), Some(Item { key: 0x1000, grip: Grip::Apart }));
        assert_eq!((holder_of(LEFT_HAND), holder_of(RIGHT_HAND)), (Some(7), Some(9)), "each hand's holder, held or not");
        assert_eq!(item(LEFT_HAND), None, "an empty hand has no item");
        assert_eq!((kind(RIGHT_HAND), kind(LEFT_HAND)), (Held::Melee, Held::Nothing));
        let dl1_gun = Line { from: [0.0; 3], along: [0.0, 0.0, 1.0], length: Some(0.25), edge: None };
        note(Noted::of(RIGHT_HAND, 0x1000, Held::Gun, None, [None; 2], false));
        assert_eq!(line(RIGHT_HAND, &Lines { gun: Some(dl1_gun), melee: None }), Some(dl1_gun));
        assert_eq!(line(RIGHT_HAND, &FROM_MODELS), None, "a gun's line is the game's own or none");
        note(Noted::of(RIGHT_HAND, 0, Held::Nothing, None, [None; 2], false));
    }

    #[test]
    fn a_throwable_is_held_and_lined_as_the_melee_weapon_it_was_reported_as() {
        let _serial = NOTING.locked();
        let dl1 = Lines { gun: None, melee: Some(Line { from: [0.0; 3], along: [0.0, 1.0, 0.0], length: None, edge: Some([0.0, 0.0, 1.0]) }) };
        for held in [Held::Gun, Held::Melee, Held::Blade, Held::Nothing] {
            for (throwable, throwing) in [(false, false), (true, false), (false, true), (true, true)] {
                let now = reported(held, throwable, throwing);
                assert_eq!(now == Held::Throwable, held == Held::Melee && (throwable || throwing), "{held:?}");
                // What the hand rig does with it is unchanged: the grip, a gun's hold.
                assert_eq!((now.grips(), now == Held::Gun), (held.grips(), held == Held::Gun), "{held:?}");
                // The line it had before.
                let weapon = if held == Held::Nothing { 0 } else { 0x1000 };
                note(Noted::of(RIGHT_HAND, weapon, held, None, [None; 2], false));
                let before = line(RIGHT_HAND, &dl1);
                note(Noted::of(RIGHT_HAND, weapon, now, None, [None; 2], false));
                assert_eq!(line(RIGHT_HAND, &dl1), before, "{held:?} {throwable} {throwing}");
            }
        }
        note(Noted::of(RIGHT_HAND, 0, Held::Nothing, None, [None; 2], false));
    }
}
