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

/// Leaves of a logged HUD frame: health's medkit back, its count, the quests' title, text and icon.
fn leaves() -> Vec<Leaf> {
    let leaf = |piece, text, at: [f32; 2], size: [f32; 2]| {
        let rect = [at[0], at[1], at[0] + size[0], at[1] + size[1]];
        Leaf { piece, text, at, rect, drawn: rect, last: false }
    };
    vec![
        leaf(Some(3), false, [185.9, 718.8], [86.8, 33.0]),
        leaf(Some(3), true, [247.9, 718.8], [16.5, 31.0]),
        leaf(Some(3), true, [417.3, 669.3], [16.5, 31.0]),
        leaf(None, false, [1001.8, 685.8], [640.3, 8.3]),
        leaf(Some(2), true, [1901.1, 1109.6], [537.1, 33.0]),
        leaf(Some(2), true, [1892.8, 1142.7], [504.3, 41.8]),
        leaf(Some(2), false, [2403.0, 1146.8], [33.0, 33.0]),
    ]
}

#[test]
fn image_draws_are_the_leaf_at_their_corner() {
    let leaves = leaves();
    // Draw 61 and the quests' icon (draw 102) of that frame.
    assert_eq!(image_leaf(&leaves, 0, [185.9, 718.9]), Some(0));
    assert_eq!(image_leaf(&leaves, 1, [2403.1, 1146.7]), Some(6));
    // From past it, the snapshot a frame off: still found.
    assert_eq!(image_leaf(&leaves, 7, [185.9, 718.9]), Some(0));
    // The minimap's own geometry (draws 2-4): no leaf.
    assert_eq!(image_leaf(&leaves, 0, [1322.8, 1320.7]), None);
    // A text leaf is never an image draw's.
    assert_eq!(image_leaf(&leaves, 0, [247.9, 718.8]), None);
}

#[test]
fn text_draws_are_the_text_leaf_of_their_row() {
    let mut leaves = leaves();
    // Health's count exactly at its leaf (draw 59).
    assert_eq!(text_leaf(&leaves, [417.3, 669.3]), Some(2));
    // The quest title, right-aligned in its box, its passes 3 pixels up (draws 96-98).
    assert_eq!(text_leaf(&leaves, [2326.0, 1106.4]), Some(4));
    // The quest text, longer than its box, starting 160 pixels left of it (draws 99-101).
    assert_eq!(text_leaf(&leaves, [1731.7, 1144.7]), Some(5));
    // Nothing on that row.
    assert_eq!(text_leaf(&leaves, [100.0, 300.0]), None);
    // The row is the text's own box, not where its characters reach (an outline 33 pixels up).
    leaves[2].drawn = [401.3, 636.3, 449.8, 733.3];
    assert_eq!(text_leaf(&leaves, [417.3, 669.3]), Some(2));
}
