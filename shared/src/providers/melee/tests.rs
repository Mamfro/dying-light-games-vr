use super::*;
use monaka_core::camera::translation;

/// A segment of DL2's size (0x34 bytes), its current blade as given.
fn segment(start: [f32; 3], tip: [f32; 3]) -> Vec<f32> {
    let mut floats = vec![0.0f32; 0x34 / 4];
    floats[6..9].copy_from_slice(&start);
    floats[9..12].copy_from_slice(&tip);
    floats
}

fn blades(floats: &[f32]) -> [[f32; 3]; 4] {
    [0, 3, 6, 9].map(|i| [floats[i], floats[i + 1], floats[i + 2]])
}

#[test]
fn a_late_attack_sweeps_the_path_the_hand_took() {
    // The hand moved along x a decimetre a posing, ticks 100 to 110; the blade runs along y from it.
    *TRAIL.locked() = (0..=10).map(|k| (100 + k, translation([k as f32 * 0.1, 0.0, 0.0]))).collect();
    *REPLAY.locked() = Some(100);
    let mut floats = segment([1.0, 0.0, 0.0], [1.0, 1.0, 0.0]);
    let data = floats.as_mut_ptr() as usize;
    replay(data, 1, 0x34);
    let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5);
    let [start_was, tip_was, start_now, tip_now] = blades(&floats);
    assert!(close(start_was, [0.0, 0.0, 0.0]) && close(tip_was, [0.0, 1.0, 0.0]), "from the first posing: {floats:?}");
    assert!(close(start_now, [0.3, 0.0, 0.0]) && close(tip_now, [0.3, 1.0, 0.0]), "to three posings on");
    assert_eq!(*REPLAY.locked(), Some(103));
    // Each sweep goes on from the last until it reaches the hand, then the game's own go on.
    let mut sweeps = 1;
    while REPLAY.locked().is_some() {
        floats = segment([1.0, 0.0, 0.0], [1.0, 1.0, 0.0]);
        replay(floats.as_mut_ptr() as usize, 1, 0x34);
        sweeps += 1;
        assert!(sweeps < 10);
    }
    let [_, _, start_now, _] = blades(&floats);
    assert!(close(start_now, [1.0, 0.0, 0.0]), "caught up with the hand");
    assert_eq!(sweeps, 4, "ten posings, three at a time");
    // With nothing to replay a sweep is left as the game made it.
    let mut live = segment([1.0, 0.0, 0.0], [1.0, 1.0, 0.0]);
    let before = live.clone();
    replay(live.as_mut_ptr() as usize, 1, 0x34);
    assert_eq!(live, before);
}
