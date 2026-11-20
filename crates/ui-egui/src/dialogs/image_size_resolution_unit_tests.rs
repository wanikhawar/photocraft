//! #14782: the Image Size dialog's Resolution unit list (Pixels/Inch, Pixels/Centimetre). The dialog converts only
//! the shown number; `image.imageSize` gets pixels per inch, and an untouched resolution stays the document's own.

use craft::ui::dialog::{DialogId, StackDialog as _};
use craft::ui::patterns::form_dialogs::HostedImageSize;
use craft::ui::runtime::env::UnitKind;
use serde_json::{Value, json};

use super::*;

/// A `width` x `height` px document at `resolution` ppi.
fn app_with(width: u32, height: u32, resolution: u32) -> Box<PhotocraftApp> {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.session.execute("file.new", json!({"width": width, "height": height, "resolution": resolution})).unwrap();
    app
}

fn image_size(app: &mut PhotocraftApp) -> DialogId {
    open(app, IMAGE_COMMAND).expect("a document is open")
}

/// Writes a field as `ui.dialog.set` does.
fn set(app: &mut PhotocraftApp, id: DialogId, field: &str, value: Value) {
    let ctx = app.ctx().clone();
    app.framework.dialogs.set_json_field(&ctx, id, field, value).unwrap_or_else(|e| panic!("{field}: {e}"));
}

fn fields(app: &PhotocraftApp, id: DialogId) -> Value {
    app.framework.dialogs.get_by_id::<HostedImageSize>(id).expect("Image Size is open").json_fields().expect("it has fields")
}

/// The Resolution the dialog shows, in its current unit.
fn shown(app: &PhotocraftApp, id: DialogId) -> f64 {
    fields(app, id)["resolution"].as_f64().expect("a number")
}

fn unit(kind: UnitKind) -> Value {
    serde_json::to_value(kind).unwrap()
}

fn ok(app: &mut PhotocraftApp, id: DialogId) {
    crate::dialogs::testing::confirm(app, id).unwrap();
}

fn doc_res(app: &PhotocraftApp) -> f32 {
    app.session.active().unwrap().doc.resolution_dpi
}

/// The dialog opens in Pixels/Inch; switching to Pixels/Centimetre converts the shown value (72 ppi is 28.346) and
/// switching back shows 72 again, without changing the resolution.
#[test]
fn switching_the_unit_converts_the_shown_value_only() {
    let mut app = app_with(200, 100, 72);
    let id = image_size(&mut app);
    assert_eq!(fields(&app, id)["resolutionUnit"], unit(UnitKind::PixelsPerInch), "opens in Pixels/Inch");
    assert_eq!(shown(&app, id), 72.0);
    set(&mut app, id, "resolutionUnit", unit(UnitKind::PixelsPerCentimetre));
    assert!((shown(&app, id) - 28.346).abs() < 1e-3, "{}", shown(&app, id));
    set(&mut app, id, "resolutionUnit", unit(UnitKind::PixelsPerInch));
    assert_eq!(shown(&app, id), 72.0);
    ok(&mut app, id);
    assert_eq!(doc_res(&app), 72.0);
}

/// 10 px/cm typed in Pixels/Centimetre is 25.4 ppi on the document.
#[test]
fn a_value_typed_per_centimetre_applies_times_2_54() {
    let mut app = app_with(200, 100, 72);
    let id = image_size(&mut app);
    set(&mut app, id, "resolutionUnit", unit(UnitKind::PixelsPerCentimetre));
    set(&mut app, id, "resolution", json!(10));
    ok(&mut app, id);
    assert!((doc_res(&app) - 25.4).abs() < 1e-4, "{}", doc_res(&app));
}

/// OK with Pixels/Centimetre shown keeps the document's resolution exactly (no round trip through 28.346), with
/// nothing edited and with only Width edited; 1 ppi, the smallest value, is not clamped into a change either.
#[test]
fn an_untouched_resolution_stays_exact_per_centimetre() {
    for (ppi, width) in [(72, None), (72, Some(100)), (300, Some(150)), (1, None)] {
        let mut app = app_with(200, 100, ppi);
        let id = image_size(&mut app);
        set(&mut app, id, "resolutionUnit", unit(UnitKind::PixelsPerCentimetre));
        if let Some(w) = width {
            set(&mut app, id, "width", json!(w));
        }
        ok(&mut app, id);
        assert_eq!(doc_res(&app), ppi as f32, "{ppi} ppi, width {width:?}");
    }
}

/// Resample on with the size in inches keeps the print size: 56.693 px/cm is about 144 ppi, twice 72, so the
/// pixels double.
#[test]
fn a_resolution_typed_per_centimetre_keeps_the_print_size() {
    let mut app = app_with(200, 100, 72);
    let id = image_size(&mut app);
    set(&mut app, id, "unit", unit(UnitKind::Inches));
    set(&mut app, id, "resolutionUnit", unit(UnitKind::PixelsPerCentimetre));
    set(&mut app, id, "resolution", json!(56.693));
    ok(&mut app, id);
    let doc = &app.session.active().unwrap().doc;
    assert!((doc.resolution_dpi - 144.0).abs() < 1e-2, "{}", doc.resolution_dpi);
    assert_eq!(doc.size, photocraft_doc::Size::new(400, 200));
}

/// Drawn in the whole app: a Resolution unit list sits right of the Resolution field showing Pixels/Inch, lists
/// Pixels/Inch above Pixels/Centimetre, and picking Pixels/Centimetre shows 72 ppi as 28.346.
#[test]
fn the_unit_list_sits_right_of_resolution_and_converts_when_picked() {
    use egui::vec2;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_max_steps(8).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 200, "height": 100, "resolution": 72})).unwrap();
        PhotocraftApp::with_context(cc.egui_ctx.clone(), s, crate::Services::default())
    });
    h.run_steps(4);
    let id = image_size(h.state_mut());
    h.run_steps(6);

    let field = h.query_all_by_label("Resolution").find(|n| n.accesskit_node().value().is_some()).expect("the Resolution field").rect();
    let list = h.query_all_by_label("Resolution unit").find(|n| n.accesskit_node().value().is_some()).expect("the Resolution unit list");
    assert_eq!(list.accesskit_node().value().as_deref(), Some("Pixels/Inch"));
    let rect = list.rect();
    assert!(rect.left() >= field.right() - 1.0 && (rect.center().y - field.center().y).abs() < 10.0, "right of the field: {field:?} {rect:?}");
    list.click();
    h.run_steps(4);

    let top = |h: &Harness<'_, Box<PhotocraftApp>>, label: &str| h.query_all_by_label(label).map(|n| n.rect().top()).fold(f32::NAN, f32::max);
    let (inch, centimetre) = (top(&h, "Pixels/Inch"), top(&h, "Pixels/Centimetre"));
    assert!(inch < centimetre, "Pixels/Inch is listed first: {inch} {centimetre}");
    h.query_all_by_label("Pixels/Centimetre").last().expect("Pixels/Centimetre is listed").click();
    h.run_steps(4);

    assert_eq!(fields(h.state(), id)["resolutionUnit"], unit(UnitKind::PixelsPerCentimetre));
    let drawn: Vec<String> = h.query_all_by_label("Resolution").filter_map(|n| n.accesskit_node().value().map(|v| v.to_string())).collect();
    assert!(drawn.iter().any(|v| v.parse::<f64>().is_ok_and(|v| (v - 28.346).abs() < 1e-3)), "shows 28.346: {drawn:?}");
}
