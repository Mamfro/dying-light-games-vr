use super::*;

#[test]
fn hud_matrices_place_quads_and_leave_text_in_pixels() {
    // Logged matrices: a minimap icon (its corner where the map saw it, 0.885 x 0.245), a rotated
    // minimap element, and quest text (pixels to the screen).
    let icon = [2.414, 0.0, 0.0, 2451.858, 0.0, -2.414, 0.0, 1640.567, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 3191.590];
    let Placement::At([x, y]) = placement_of(&icon) else { panic!("{:?}", placement_of(&icon)) };
    assert!((x - 0.884).abs() < 0.002 && (y - 0.243).abs() < 0.002, "{x} {y}");
    let rotated = [-1.619, 1.791, 0.0, 2194.469, 1.791, 1.619, 0.0, 875.911, 0.0, 0.0, 0.0, 1.0, -0.0, 0.0, -1.0, 3191.590];
    assert!(matches!(placement_of(&rotated), Placement::At(_)));
    let text = [2.414, 0.0, 0.0, -3191.590, 0.0, -2.414, 0.0, 3191.590, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 3191.590];
    assert_eq!(placement_of(&text), Placement::Pixels);
    // Another shader's buffer (its last row is not the HUD's).
    let other = [-1.106, 1.543, 0.0, 0.684, 1.619, -1.791, 0.0, 2517.355, -1.791, -1.619, 0.0, -936.189, 0.0, 0.0, 0.0, 1.0];
    assert_eq!(placement_of(&other), Placement::Unknown);
}
