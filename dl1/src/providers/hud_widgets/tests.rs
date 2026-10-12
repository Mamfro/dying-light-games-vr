use super::*;

#[test]
fn the_minimap_is_its_frame_and_the_rest_their_leaves_union() {
    let frame = [3100.0, 78.0, 3585.0, 561.0];
    let icon = [3300.0, 300.0, 3332.0, 332.0];
    let far_icon = [2500.0, 900.0, 2532.0, 932.0];
    // The minimap: the frame, whatever order the leaves come in; a far icon does not grow it.
    let mut b = piece_box(MINIMAP, None, icon);
    for leaf in [frame, far_icon, icon] {
        b = piece_box(MINIMAP, Some(b), leaf);
    }
    assert_eq!(b, frame);
    // Another piece: the union.
    assert_eq!(piece_box(2, Some(frame), far_icon), [2500.0, 78.0, 3585.0, 932.0]);
}
