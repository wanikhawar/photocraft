//! The Retouch options bar's selection family: Quick Selection and Object Selection.

use craft::behavior::IconId;
use craft::ui::buttons::{Button, IconButton};
use craft::ui::contract::{Entry, Props as _, Show as _, Trigger, clicked};
use craft::ui::display::Divider;
use craft::ui::menu::Menu;
use craft::ui::runtime::access::PopupKind;
use craft::ui::text::RangeAlert;
use serde_json::json;

use super::heal::check;
use crate::PhotocraftApp;
use crate::option_cells::hint;
use crate::state::ObjectFinderMode;

/// The Select Subject button both selection tools carry; `empty` is the message when nothing is found.
fn select_subject(ui: &mut egui::Ui, app: &mut PhotocraftApp, empty: &str) {
    if clicked(&Button::new(tl!("Select Subject")).show(ui)) {
        let r = app.run("select.subject", json!({}));
        crate::retouch_ui::report_smart_result(app, r, empty);
    }
}

/// Quick Selection's options.
pub(super) fn quick_selection(ui: &mut egui::Ui, app: &mut PhotocraftApp) -> Option<RangeAlert> {
    let o = &mut app.ui.tool_options;
    check(ui, tl!("Sample All Layers"), &mut o.sample_all.quick_selection, true, None);
    check(ui, tl!("Enhance Edge"), &mut o.enhance_edge, true, None);
    hint(ui, crate::i18n::fmt(tl!("{key} to subtract"), &[("key", &crate::shortcuts::pretty("Alt"))]));
    Divider::vertical().show(ui);
    select_subject(ui, app, tl!("No subject found — try Object Selection or the Lasso"));
    None
}

/// Object Selection's options: the selection operation (#12853), Sample All Layers, Hard Edge,
/// and the Object Finder's Refresh button and gear menu (#12857).
pub(super) fn object_selection(ui: &mut egui::Ui, app: &mut PhotocraftApp) -> Option<RangeAlert> {
    crate::panels::options_bar::selection::operation_segments(app, ui, true);
    Divider::vertical().show(ui);
    let o = &mut app.ui.tool_options;
    check(ui, tl!("Sample All Layers"), &mut o.sample_all.object_selection, true, None);
    check(ui, tl!("Hard Edge"), &mut o.object_hard_edge, true, None);
    let tip = tl!("Click to refresh object finder.");
    if clicked(&IconButton::new(IconId::new("rotate_cw")).label(tip).tooltip(tip).show(ui)) {
        crate::object_finder::refresh(app);
    }
    object_finder_gear(ui, app);
    hint(ui, tl!("Drag a rectangle around the object"));
    Divider::vertical().show(ui);
    select_subject(ui, app, tl!("No subject found — try a rectangle around the object"));
    None
}

/// A row of Object Selection's gear menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Gear {
    ObjectFinder,
    ObjectSubtract,
    Mode(ObjectFinderMode),
}

/// The gear after the Refresh button (#12857): Object Finder and Object Subtract (stored only),
/// then the Object Finder Mode, all saved with the tool options.
fn object_finder_gear(ui: &mut egui::Ui, app: &mut PhotocraftApp) {
    let label = tl!("Set additional options").to_string();
    let button = IconButton::new(IconId::new("settings")).label(label.clone()).tooltip(label.clone()).popup(PopupKind::Menu).show(ui);
    let o = &app.ui.tool_options;
    let entries = [
        Entry::new(Gear::ObjectFinder, tl!("Object Finder").to_string()).checked(o.object_finder),
        Entry::new(Gear::ObjectSubtract, tl!("Object Subtract").to_string()).checked(o.object_subtract),
        Entry::separator(),
        Entry::heading(tl!("Object Finder Mode").to_string()),
        Entry::new(Gear::Mode(ObjectFinderMode::Auto), tl!("Auto Refresh").to_string()).checked(o.object_finder_mode == ObjectFinderMode::Auto),
        Entry::new(Gear::Mode(ObjectFinderMode::Manual), tl!("Manual Refresh").to_string()).checked(o.object_finder_mode == ObjectFinderMode::Manual),
    ];
    let Some(picked) = Menu::new(label).opened_by(&button, Trigger::Press).pick(ui, &entries).picked else { return };
    let o = &mut app.ui.tool_options;
    match picked {
        Gear::ObjectFinder => o.object_finder = !o.object_finder,
        Gear::ObjectSubtract => o.object_subtract = !o.object_subtract,
        Gear::Mode(mode) => o.object_finder_mode = mode,
    }
}
