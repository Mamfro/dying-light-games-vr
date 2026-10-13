//! What the arms held as read without the game type: the hand rig's snapshot at each posing
//! (`monaka_body::held::wielded`) against the melee snapshot it replaced, taken at each arms
//! callback from what the swings took the arms to hold.

use super::tests::NOTING;
use super::*;
use monaka_body::hand_rig::{self, Climbing, HandRig, Hands, Moment};
use monaka_producer::game::{Adapter, Game};

/// What the swings took the arms to hold, as the arms callback kept it before (its last weapon
/// not nothing kept across the update).
fn swung_as(held: Kind, last: &mut Kind) -> Kind {
    if held != Kind::Nothing {
        *last = held;
        return held;
    }
    if *last == Kind::Gun { Kind::Gun } else { held }
}

/// The melee snapshot as it was kept: set at every arms callback with the rig on, none before.
struct Before {
    held: Option<Kind>,
    last: Kind,
}

impl Before {
    fn update(&mut self, held: Kind, bow: bool) {
        let swung = swung_as(held, &mut self.last);
        self.held = Some(if bow { Kind::Gun } else { swung });
    }
}

/// A Chrome game: its hands as the arms callback notes them.
struct Chromish;
impl Game for Chromish {
    const MANIFESTS: &'static [monaka_core::manifest::Manifest] = &[];
    const ADAPTER: Adapter<Self> = Adapter { components: &[] };
    type Build = ();
    type Frame = ();
    fn prepare(_: &'static monaka_producer::Session, _: &'static monaka_core::manifest::Manifest, _: &monaka_core::options::Options) -> Result<(), monaka_producer::Rejection> {
        Ok(())
    }
}
impl HandRig for Chromish {
    const RIG: hand_rig::Rig = crate::providers::fpp::chrome_rig(monaka_body::BoneAxes::DYING_LIGHT_1, None, Climbing::Animated);
    fn held(side: usize) -> Kind {
        kind(side)
    }
    fn item(side: usize) -> Option<Item> {
        item(side)
    }
}

/// One arms callback: what the aim hand's weapon slot holds, a bow in the left hand's slot, the
/// slot holding a throwable (or what a throw under way throws), and whether the rig posed the arms.
#[derive(Clone, Copy, Debug)]
struct Callback {
    weapon: Kind,
    bow: bool,
    throwable: bool,
    posed: bool,
}

/// Why a read differs from the one before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Differs {
    /// The rig did not pose the arms at the latest callback: the snapshot is of an earlier one.
    NotPosed,
    /// Empty hands after a gun: the swings still took them to hold the gun.
    GunPutAway,
    /// A weapon in the left aim hand beside a bow: the bow is in no hand of its own.
    BowBesideLeftWeapon,
}

/// A run of callbacks, the same each run: every weapon, with and without the bow, a throwable and
/// a posing skipped now and then.
fn callbacks() -> Vec<Callback> {
    let mut seed = 0x2545_f491u32;
    let mut next = |n: u32| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed % n
    };
    (0..400)
        .map(|_| Callback {
            weapon: [Kind::Nothing, Kind::Gun, Kind::Melee, Kind::Nothing][next(4) as usize],
            bow: next(5) == 0,
            throwable: next(4) == 0,
            posed: next(6) != 0,
        })
        .collect()
}

#[test]
fn every_read_is_the_kind_the_melee_snapshot_gave_but_the_listed_ones() {
    let _serial = NOTING.locked();
    for aim in [LEFT_HAND, RIGHT_HAND] {
        let mut before = Before { held: None, last: Kind::Nothing };
        // The new snapshot's reads: none until a posing (the rig's earlier posings in this run
        // are another aim's, so the first posing here is the one compared).
        let mut posed_once = false;
        let mut seen = [0u32; 4];
        for (i, callback) in callbacks().into_iter().enumerate() {
            let weapon = if callback.weapon == Kind::Nothing { 0 } else { 0x1000 + i };
            let kind = reported(callback.weapon, callback.weapon == Kind::Melee && callback.throwable, false);
            note(Noted::of(aim, weapon, kind, callback.bow.then_some(0x2000), [Some(7), Some(9)], false));
            let last = before.last;
            before.update(callback.weapon, callback.bow);
            if callback.posed {
                let _ = hand_rig::capture::<Chromish>(Moment { hands: Hands::Attached { right: 0, targets: [None; 2] }, fingers: false });
                posed_once = true;
            }
            if !posed_once {
                continue;
            }
            let now = monaka_body::held::wielded(aim);
            let differs = if !callback.posed {
                Some(Differs::NotPosed)
            } else if !callback.bow && callback.weapon == Kind::Nothing && last == Kind::Gun {
                Some(Differs::GunPutAway)
            } else if callback.bow && aim == LEFT_HAND && callback.weapon != Kind::Nothing {
                Some(Differs::BowBesideLeftWeapon)
            } else {
                None
            };
            match differs {
                None => {
                    assert_eq!(now, before.held, "callback {i} {callback:?} aim {aim}");
                    seen[0] += 1;
                }
                Some(Differs::GunPutAway) => {
                    assert_eq!((before.held, now), (Some(Kind::Gun), Some(Kind::Nothing)), "callback {i}");
                    seen[1] += 1;
                }
                Some(Differs::BowBesideLeftWeapon) => {
                    assert_eq!((before.held, now), (Some(Kind::Gun), Some(callback.weapon)), "callback {i}");
                    seen[2] += 1;
                }
                // The read is the one at the latest posing; the next posing catches up.
                Some(Differs::NotPosed) => seen[3] += 1,
            }
        }
        assert!(seen.iter().all(|&n| n > 0) || (aim == RIGHT_HAND && seen[2] == 0), "every case met: {seen:?}");
    }
    note(Noted::of(RIGHT_HAND, 0, Kind::Nothing, None, [None; 2], false));
}
