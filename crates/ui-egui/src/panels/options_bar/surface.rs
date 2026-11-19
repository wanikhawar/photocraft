//! The Brush's and Pencil's Surface button: a popover with Surface Color (the paint takes on the photo's shading:
//! Strength, Detail, Sample) and Follow Perspective (dabs follow the stored Vanishing Point planes), plus Clear
//! Planes. Edits go through `tools.setBrush`, one history step per gesture; Clear Planes runs
//! `perspective.clearPlanes`.

use craft::behavior::IconId;
use craft::ui::buttons::{Button, IconButton, PopupMark};
use craft::ui::choice::Checkbox;
use craft::ui::contract::{Bind, EditResponse, Entry, PopupKind, Props as _, Show as _, ShowContainer as _};
use craft::ui::display::{Divider, Text};
use craft::ui::form::{Field, SliderField};
use craft::ui::layout_components::Row;
use craft::ui::pickers::Select;
use craft::ui::popover::Popover;
use craft::ui::runtime::env::{Label, UnitKind};
use photocraft_paint::surface::{DETAIL_RANGE, SampleMode};
use serde_json::json;

use crate::PhotocraftApp;

/// Whether the Surface popover is open, kept by the framework per viewer.
#[derive(Clone, Copy, Default)]
struct SurfaceOpen(bool);

fn label(text: &str) -> Label {
    Label::from(text.to_string())
}

/// The pass's edits: whether any, and the coalescing key of the last, as the paint bar keys its own, so one gesture
/// is one history step.
#[derive(Default)]
struct Edits {
    any: bool,
    key: Option<String>,
}

impl Edits {
    /// Takes `edit`'s events; whether it held any.
    fn note<T>(&mut self, edit: &EditResponse<T>) -> bool {
        let mut any = false;
        for event in edit.events.iter() {
            self.key = crate::history_keys::coalesce("brush", event);
            any = true;
        }
        self.any |= any;
        any
    }
}

/// The Surface button and its popover, after the paint bar of the Brush and the Pencil.
pub(super) fn options(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let before = app.session.tools.brush.clone();
    let mut s = before.surface.clone();
    let clear_enabled = crate::menus::is_enabled(app, "perspective.clearPlanes");
    let mut edits = Edits::default();
    let mut clear = false;
    Row::new().show(ui, |ui, cx| {
        let open = &mut cx.memory.shown::<SurfaceOpen>(cx.id).0;
        let button = IconButton::new(IconId::new("grid"))
            .label(label(tl!("Surface options")))
            .popup(PopupKind::Dialog)
            .popup_mark(PopupMark::Menu)
            .expanded(*open)
            .show(ui)
            .response;
        if craft::ui::contract::clicked(&button) {
            *open = !*open;
        }
        Popover::new(label(tl!("Surface options")), Bind::bound(open)).opened_by(&button).show(ui, |ui, _| {
            Text::new(label(tl!("Surface"))).show(ui);
            edits.note(&Checkbox::new(Bind::bound(&mut s.color)).label(label(tl!("Surface Color"))).show(ui));
            let mut strength = (f64::from(s.strength) * 100.0).round().clamp(0.0, 100.0);
            let edit = SliderField::new(label(tl!("Strength")), Bind::bound(&mut strength), 0.0..=100.0)
                .step(1.0)
                .unit_kind(UnitKind::Percent)
                .enabled(s.color)
                .show(ui);
            if edits.note(&edit) {
                s.strength = ((strength / 100.0) as f32).clamp(0.0, 1.0);
            }
            let (lo, hi) = (f64::from(*DETAIL_RANGE.start()), f64::from(*DETAIL_RANGE.end()));
            let mut detail = f64::from(s.detail).round().clamp(lo, hi);
            let edit =
                SliderField::new(label(tl!("Detail")), Bind::bound(&mut detail), lo..=hi).step(1.0).unit_kind(UnitKind::Pixels).enabled(s.color).show(ui);
            if edits.note(&edit) {
                s.detail = (detail as f32).clamp(*DETAIL_RANGE.start(), *DETAIL_RANGE.end());
            }
            let modes = [Entry::new(SampleMode::AllLayers, label(tl!("All Layers"))), Entry::new(SampleMode::CurrentLayer, label(tl!("Current Layer")))];
            Field::new(label(tl!("Sample"))).show(ui, |ui, _| {
                edits.note(&Select::new(label(tl!("Sample")), Bind::bound(&mut s.sample), &modes).enabled(s.color).show(ui));
            });
            Divider::horizontal().show(ui);
            edits.note(&Checkbox::new(Bind::bound(&mut s.perspective)).label(label(tl!("Follow Perspective"))).show(ui));
            if craft::ui::contract::clicked(&Button::new(label(tl!("Clear Planes"))).enabled(clear_enabled).show(ui).response) {
                clear = true;
            }
        });
    });
    if edits.any {
        let mut after = before.clone();
        after.surface = s;
        crate::brush_panel::commit_keyed(app, &before, &after, edits.key);
    }
    if clear {
        crate::brush_panel::run_or_status(app, "perspective.clearPlanes", json!({}));
    }
}
