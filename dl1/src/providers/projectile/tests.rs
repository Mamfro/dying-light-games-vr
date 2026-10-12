use super::*;

#[test]
fn an_asked_shot_is_its_weapons_answer() {
    let game = Flight { from: [1.0, 2.0, 3.0], velocity: [0.0, 0.0, -50.0] };
    let asked = Asked { weapon: Weapon::Bow, aim: None };
    launch::aim_shots(Weapon::Bow, None);
    assert_eq!(asked.placed(game), None, "no answer: as the game fires it");
    launch::aim_shots(Weapon::Bow, Some(|_| Some(launch::Shot { from: [0.0; 3], direction: [1.0, 0.0, 0.0], speed: Some(70.0) })));
    assert_eq!(asked.placed(game), Some(Flight { from: [0.0; 3], velocity: [70.0, 0.0, 0.0] }));
    assert_eq!(Asked { weapon: Weapon::Gun, aim: None }.placed(game), None, "the gun's answer is its own");
    assert_eq!(asked.placed(Flight { velocity: [0.0; 3], ..game }), None, "a projectile with no velocity is left as fired");
    launch::aim_shots(Weapon::Bow, None);
}
