use crate::canvas::testing::{ToolEvent, tool_event_response};
use crate::{PhotocraftApp, state::Tool};
use craft::behavior::{Cursor, CursorIcon, PaintedCursor};
use egui::Modifiers;
use serde_json::json;

#[test]
fn type_cursor_routes_and_precise_policy() {
    for tool in [Tool::Type, Tool::HorizontalTypeMask, Tool::VerticalType, Tool::VerticalTypeMask] {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width":1600,"height":900})).unwrap();
        app.sync_views();
        app.ui.extras.rulers = false;
        app.ui.tool = tool;
        let expected = match tool {
            Tool::VerticalType => Cursor::from(PaintedCursor::outline(super::cursor::vertical_type_rings())),
            Tool::VerticalTypeMask => Cursor::System(CursorIcon::VerticalText),
            _ => Cursor::System(CursorIcon::Text),
        };
        for precise in [false, true] {
            if precise {
                app.run("prefs.set", json!({"path":"cursors.other","value":"precise"})).unwrap();
            }
            let response = tool_event_response(&mut app, ToolEvent::Move { x: 800.0, y: 450.0, pressure: 1.0 }, Modifiers::NONE);
            assert_eq!(response.cursor, Some(expected.clone()), "{tool:?}, precise={precise}");
        }
    }
}

#[test]
fn sideways_geometry_has_offset_dotted_box() {
    let rings = super::cursor::vertical_type_rings();
    assert_eq!(rings.len(), 13);
    assert_eq!(rings[0].len(), 12);
    assert!(rings[0].contains(&egui::pos2(-5.0, -0.5)));
    assert!(rings[0].contains(&egui::pos2(5.0, 0.5)));
    for corner in [(-9.0, -4.0), (9.0, -4.0), (-9.0, 12.0), (9.0, 12.0)] {
        assert!(rings[1..].iter().any(|ring| {
            let center = ring.iter().fold(egui::Vec2::ZERO, |sum, p| sum + p.to_vec2()) / 4.0;
            center == egui::vec2(corner.0, corner.1)
        }));
    }
}
