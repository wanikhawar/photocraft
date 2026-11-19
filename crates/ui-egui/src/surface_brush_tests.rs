//! The Brush's Surface options: the popover's toggles reach `tools.setBrush`, and the brush cursor
//! takes the dab's shape from the stored perspective planes (Follow Perspective) or stays plain.

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;

fn app(tool: Tool) -> Box<PhotocraftApp> {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width": 240, "height": 160})).unwrap();
    app.ui.tool = tool;
    crate::paint_mouse::sync_tool_brush(&mut app);
    app
}

fn has_fonts(ctx: &egui::Context) -> bool {
    ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into())))
}

/// The options bar with the Surface popover open; `None` without the app's fonts (the bar draws nothing then).
fn open_popover(app: Box<PhotocraftApp>) -> Option<Harness<'static, Box<PhotocraftApp>>> {
    let mut h = Harness::builder().with_context(app.ctx().clone()).with_size(vec2(1800.0, 1000.0)).build_ui_state(
        |ui, app: &mut Box<PhotocraftApp>| {
            if has_fonts(ui.ctx()) {
                crate::panels::options_bar(app, ui);
            }
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::AppTheme::studio());
    h.run_steps(4);
    if !has_fonts(&h.ctx) {
        return None;
    }
    h.get_by_label("Surface options").click();
    h.run_steps(3);
    Some(h)
}

/// The popover's checkbox named `label`.
fn checkbox<'a>(h: &'a Harness<'static, Box<PhotocraftApp>>, label: &str) -> egui_kittest::Node<'a> {
    h.get_all_by_label(label).find(|n| n.accesskit_node().role() == egui::accesskit::Role::CheckBox).unwrap_or_else(|| panic!("{label}"))
}

#[test]
fn the_popover_toggles_reach_the_brush_settings() {
    for tool in [Tool::Brush, Tool::Pencil] {
        let Some(mut h) = open_popover(app(tool)) else { return };
        assert!(!h.state().session.tools.brush.surface.color);
        checkbox(&h, "Surface Color").click();
        h.run_steps(3);
        assert!(h.state().session.tools.brush.surface.color, "{tool:?}: Surface Color");
        checkbox(&h, "Follow Perspective").click();
        h.run_steps(3);
        assert!(h.state().session.tools.brush.surface.perspective, "{tool:?}: Follow Perspective");
    }
}

#[test]
fn strength_detail_and_sample_are_greyed_until_surface_color_is_on() {
    let Some(h) = open_popover(app(Tool::Brush)) else { return };
    for label in ["Strength", "Detail", "Sample"] {
        let nodes: Vec<_> = h.get_all_by_label(label).collect();
        assert!(!nodes.is_empty(), "{label}");
        assert!(nodes.iter().any(|n| n.accesskit_node().is_disabled()), "{label} is greyed");
    }
}

/// Picking from the popover's Sample list sets the option and leaves the popover open.
#[test]
fn the_sample_list_in_the_popover_picks_its_row_and_keeps_the_popover_open() {
    use photocraft_paint::surface::SampleMode;
    let Some(mut h) = open_popover(app(Tool::Brush)) else { return };
    checkbox(&h, "Surface Color").click();
    h.run_steps(3);
    let sample = h.get_all_by_label("Sample").find(|n| n.accesskit_node().role() == egui::accesskit::Role::ComboBox).expect("the Sample list");
    sample.click();
    h.run_steps(4);
    h.get_all_by_label("Current Layer").next_back().expect("the Current Layer row").click();
    h.run_steps(4);
    assert_eq!(h.state().session.tools.brush.surface.sample, SampleMode::CurrentLayer);
    assert!(h.query_by_label("Surface Color").is_some(), "the popover stayed open");
}

#[test]
fn each_edit_is_one_set_brush() {
    let Some(mut h) = open_popover(app(Tool::Brush)) else { return };
    let count = |h: &Harness<'static, Box<PhotocraftApp>>| h.state().session.journal.iter().filter(|(id, _)| id == "tools.setBrush").count();
    let before = count(&h);
    checkbox(&h, "Surface Color").click();
    h.run_steps(3);
    assert_eq!(count(&h), before + 1);
}

#[test]
fn the_cursor_follows_a_plane_and_stays_round_off_it() {
    let mut app = app(Tool::Brush);
    app.run("tools.setBrush", json!({"brush": {"surface": {"perspective": true}}})).unwrap();
    // Off every plane (none stored yet): the plain outline.
    assert!(crate::brush_cursor::surface_shape(&app, Tool::Brush, [120.0, 100.0]).is_none());
    app.run("perspective.setPlanes", json!({"planes": [[[70, 60], [170, 60], [220, 150], [20, 150]]]})).unwrap();
    let on = crate::brush_cursor::surface_shape(&app, Tool::Brush, [120.0, 100.0]).expect("on the floor plane");
    assert!(on.radius > 0.0 && on.proj_scale > 0.0 && on.proj_scale <= 1.0, "{on:?}");
    assert!(crate::brush_cursor::surface_shape(&app, Tool::Brush, [5.0, 5.0]).is_none(), "off the plane");
    // Follow Perspective off, or another tool: always plain.
    assert!(crate::brush_cursor::surface_shape(&app, Tool::Eraser, [120.0, 100.0]).is_none());
    app.run("tools.setBrush", json!({"brush": {"surface": {"perspective": false}}})).unwrap();
    assert!(crate::brush_cursor::surface_shape(&app, Tool::Brush, [120.0, 100.0]).is_none());
}

/// The pose is the dab's size over the tip's and squashed across its angle; a round dab of the tip's own size is
/// upright.
#[test]
fn the_surface_pose_scales_and_squashes_the_tip() {
    use photocraft_engine::brush_cmds::CursorShape;
    let plain = crate::brush_cursor::surface_pose(CursorShape { radius: 10.0, proj_angle: 0.0, proj_scale: 1.0 }, 10.0);
    assert!((plain.x_axis.x - 1.0).abs() < 1e-9 && plain.x_axis.y.abs() < 1e-9 && (plain.y_axis.y - 1.0).abs() < 1e-9);
    // Half the size, squashed to 0.5 along x: x shrinks to 0.25, y to 0.5.
    let pose = crate::brush_cursor::surface_pose(CursorShape { radius: 5.0, proj_angle: 0.0, proj_scale: 0.5 }, 10.0);
    assert!((pose.x_axis.x - 0.25).abs() < 1e-9, "{:?}", pose.x_axis);
    assert!((pose.y_axis.y - 0.5).abs() < 1e-9, "{:?}", pose.y_axis);
    // A degenerate base keeps the tip's size.
    let base0 = crate::brush_cursor::surface_pose(CursorShape { radius: 5.0, proj_angle: 0.0, proj_scale: 1.0 }, 0.0);
    assert!((base0.x_axis.x - 1.0).abs() < 1e-9);
}

#[test]
fn ui_inspect_reports_the_cursor_shape() {
    use crate::control::{ControlRequest, Reply, handle};
    let ctx = egui::Context::default();
    let mut app = app(Tool::Brush);
    let inspect = |app: &mut PhotocraftApp| {
        let (req, _rx) = ControlRequest::new("ui.inspect", json!({}));
        let Reply::Done(v) = handle(app, &ctx, &req) else { panic!("ui.inspect replies at once") };
        v
    };
    let before = inspect(&mut *app);
    assert_eq!(before["ok"], true, "{before}");
    assert!(before["result"]["brushCursor"].is_null());
    app.surface_cursor = Some(photocraft_engine::brush_cmds::CursorShape { radius: 12.0, proj_angle: 0.5, proj_scale: 0.4 });
    let v = inspect(&mut *app);
    assert_eq!(v["result"]["brushCursor"]["projScale"], json!(0.4f32));
    assert_eq!(v["result"]["brushCursor"]["radius"], json!(12.0f32));
}
