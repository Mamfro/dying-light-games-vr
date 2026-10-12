use super::*;

#[test]
fn the_power_follows_the_games_curve() {
    assert_eq!(power(0.0, 2.0), 0.0);
    assert!((power(0.5, 2.0) - 1.5).abs() < 1e-6);
    assert_eq!(power(1.0, 2.0), 2.0);
    assert_eq!(power(3.0, 2.0), 2.0, "no more than a full draw");
}
