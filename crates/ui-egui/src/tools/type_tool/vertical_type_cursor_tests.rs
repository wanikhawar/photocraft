//! The vertical type tools' cursors (#12063): the Vertical Type tool shows an I-beam turned on its
//! side inside a dotted box, over the canvas, the pasteboard and existing text; the Vertical Type
//! Mask tool the system's sideways I-beam alone. The horizontal tools keep the upright I-beam, and
//! Preferences › Cursors › Other Cursors = Precise treats the vertical tools as it treats the Type
//! tool. Driven through real egui input on the real canvas; the assertions read the cursor the tool
//! asked the tool host for.

use craft::behavior::{Cursor, CursorIcon, PaintedCursor};
use egui::{Modifiers, Pos2, pos2};
use egui_kittest::Harness;
use serde_json::json;

use crate::PhotocraftApp;
use crate::guide_cursor_tests::{harness, hover};
use crate::state::Tool;

/// A point on the canvas away from the harness's guide at x = 200.
const AT: [f32; 2] = [100.0, 150.0];

/// The cursor the tool host was asked for with the pointer over document point (`x`, `y`).
fn cursor_at(h: &mut Harness<'static, PhotocraftApp>, x: f32, y: f32) -> Option<Cursor> {
    hover(h, x, y, Modifiers::NONE);
    h.state().canvas_tools.last_response.as_ref().and_then(|r| r.cursor.clone())
}

fn cursor(h: &mut Harness<'static, PhotocraftApp>) -> Option<Cursor> {
    cursor_at(h, AT[0], AT[1])
}

fn set_other_cursors(h: &mut Harness<'static, PhotocraftApp>, value: &str) {
    h.state_mut().run("prefs.set", json!({"path": "cursors.other", "value": value})).unwrap();
    h.run_steps(2);
}

/// The rings of a painted outline cursor.
fn rings(c: &Option<Cursor>) -> Vec<Vec<Pos2>> {
    match c {
        Some(Cursor::Painted(PaintedCursor::Outline { rings, .. })) => rings.clone(),
        other => panic!("expected a painted outline cursor, got {other:?}"),
    }
}

/// Whether (`x`, `y`) points from the hotspot falls inside one of the closed `rings`.
fn covered(rings: &[Vec<Pos2>], x: f32, y: f32) -> bool {
    rings.iter().any(|ring| {
        let mut inside = false;
        for (i, a) in ring.iter().enumerate() {
            let b = ring[(i + 1) % ring.len()];
            if (a.y > y) != (b.y > y) && x < a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x) {
                inside = !inside;
            }
        }
        inside
    })
}

#[test]
fn vertical_type_shows_a_sideways_i_beam_in_a_dotted_box() {
    let mut h = harness(Tool::VerticalType);
    let c = cursor(&mut h);
    let r = rings(&c);
    // The stem runs across through the hotspot, not up and down.
    assert!(covered(&r, 0.0, 0.0), "the stem under the hotspot");
    assert!(covered(&r, -4.0, 0.0) && covered(&r, 4.0, 0.0), "the stem left and right of the hotspot");
    assert!(!covered(&r, 0.0, -2.0) && !covered(&r, 0.0, 2.0), "nothing above or below the hotspot");
    // The serifs stand up and down at the stem's ends.
    assert!(covered(&r, -5.5, -2.0) && covered(&r, 5.5, 2.0), "the serifs");
    // The dotted box: its corners 9 left/right of the hotspot, 4 above and 12 below.
    for (dx, dy) in [(-9.0, -4.0), (9.0, -4.0), (-9.0, 12.0), (9.0, 12.0)] {
        assert!(covered(&r, dx, dy), "a box corner dot at ({dx}, {dy})");
    }
    // Dotted, not a line: gaps between the dots on the bottom edge.
    assert!(!covered(&r, -6.0, 12.0) && !covered(&r, 6.0, 12.0), "gaps between the dots");
    assert_ne!(c, cursor(&mut harness(Tool::Type)), "not the horizontal tool's cursor");
}

#[test]
fn vertical_type_keeps_the_boxed_cursor_on_the_pasteboard_and_over_text() {
    let mut h = harness(Tool::VerticalType);
    let canvas = cursor(&mut h);
    assert_eq!(cursor_at(&mut h, -40.0, 150.0), canvas, "the pasteboard left of the document");
    h.state_mut().run("type.create", json!({"text": "Hello", "size": 40, "x": 60, "y": 120})).unwrap();
    h.run_steps(2);
    assert_eq!(cursor_at(&mut h, 80.0, 110.0), canvas, "over existing text");
}

#[test]
fn vertical_type_mask_shows_the_sideways_i_beam_without_a_box() {
    let mut h = harness(Tool::VerticalTypeMask);
    assert_eq!(cursor(&mut h), Some(Cursor::System(CursorIcon::VerticalText)));
}

#[test]
fn the_horizontal_type_tools_keep_the_upright_i_beam() {
    for tool in [Tool::Type, Tool::HorizontalTypeMask] {
        assert_eq!(cursor(&mut harness(tool)), Some(Cursor::System(CursorIcon::Text)), "{tool:?}");
    }
}

#[test]
fn precise_other_cursors_treat_the_vertical_tools_as_the_type_tool() {
    let mut reference = harness(Tool::Type);
    let type_standard = cursor(&mut reference);
    set_other_cursors(&mut reference, "precise");
    let type_precise = cursor(&mut reference);
    for tool in [Tool::VerticalType, Tool::VerticalTypeMask] {
        let mut h = harness(tool);
        let standard = cursor(&mut h);
        set_other_cursors(&mut h, "precise");
        let shown = cursor(&mut h);
        if type_precise == type_standard {
            assert_eq!(shown, standard, "{tool:?}: Precise leaves the type cursors as they are");
        } else {
            assert_eq!(shown, type_precise, "{tool:?}: the same precise cursor as the Type tool");
        }
    }
}

#[test]
fn the_box_corner_sits_9_left_and_4_above_the_hotspot() {
    let r = super::vertical_type_rings();
    let min = r.iter().flatten().fold(pos2(f32::MAX, f32::MAX), |m, p| m.min(*p));
    // The corner dot's centre, half a dot in from the outline's extent.
    assert_eq!((min.x + 0.75, min.y + 0.75), (-9.0, -4.0));
}
