use crate::providers::engine::{PANELS, PIECES};
use eng_chr::providers::gui::{MAX_PARTS, MAX_PIECES, Piece};

#[test]
fn every_piece_goes_on_a_place_the_viewer_has() {
    assert!(PIECES.len() <= MAX_PIECES);
    for (i, piece) in PIECES.iter().enumerate() {
        assert!(monaka_core::hud::PIECES.contains(&piece.panel.name), "{}: the viewer has no place for it", piece.panel.name);
        assert!(PIECES[..i].iter().all(|p| p.panel.name != piece.panel.name), "{} twice", piece.panel.name);
        assert_eq!(PANELS[i], piece.panel, "the panels in the pieces' order");
        assert!(piece.parts.len() <= MAX_PARTS, "{}: parts past the most read", piece.panel.name);
    }
}

/// Whether piece `outer` takes in what piece `inner` reads: the same document, `inner`'s path
/// running through `outer`'s element and on into it (or the same element), into the whole element
/// or one of the parts `outer` reads.
fn takes_in(outer: &Piece, inner: &Piece) -> bool {
    if outer.document != inner.document || !inner.path.starts_with(outer.path) {
        return false;
    }
    let reads = |name: &str| outer.parts.is_empty() || outer.parts.contains(&name);
    match inner.path.get(outer.path.len()) {
        Some(next) => reads(next),
        None => inner.parts.is_empty() || inner.parts.iter().any(|p| reads(p)),
    }
}

#[test]
fn no_widget_goes_on_two_panels() {
    for (i, a) in PIECES.iter().enumerate() {
        for b in &PIECES[i + 1..] {
            assert!(!takes_in(a, b) && !takes_in(b, a), "{} and {} read the same widget", a.panel.name, b.panel.name);
        }
    }
}
