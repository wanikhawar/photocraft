//! The painting cursor of the brush tools, over the framework's `BrushCursor`.
//!
//! The app keeps one [`BrushTip`] for the brush (`PhotocraftApp::brush_tip`), rebuilt by [`sync_tip`] only when
//! a field the outline depends on changes, so the framework reuses its cursor image while the pointer moves.
//! The Pencil and the Eraser in Pencil or Block mode have no held tip: their pixel cell follows the pointer, so
//! [`cursor`] builds it per call.

use craft::behavior::kurbo::{BezPath, Rect, Vec2};
use craft::behavior::{Cursor, CursorIcon, PointerSample};
use craft::ui::canvas::ToolContext;
use craft::ui::canvas_controls::SelectionOutline;
use craft::ui::canvas_controls::brush::{BrushCursor, BrushCursorOptions, BrushTip, BrushTipOutline, PaintingCursor, TipPose, TipRings};
use craft::ui::contract::Show as _;
use photocraft_engine::brush_cmds::CursorShape;
use photocraft_engine::prefs::PaintingCursor as PaintingPref;
use photocraft_paint::brush::{BrushSettings, Control, TipShape};
use photocraft_paint::dynamics::{StepInput, apply_pose, control_angle};

use crate::PhotocraftApp;
use crate::state::Tool;

/// The brush fields a tip's outline depends on. The size is part of it only for a plain round tip, whose radii
/// scale with it; a traced outline is in radius units.
#[derive(Clone, PartialEq)]
pub(crate) struct TipKey {
    tip: TipShape,
    angle: f32,
    roundness: f32,
    hardness: f32,
    flip_x: bool,
    flip_y: bool,
    /// The size of a plain round tip, 0 for a traced one.
    size: f32,
}

impl TipKey {
    fn of(b: &BrushSettings) -> Self {
        Self {
            tip: b.tip.clone(),
            angle: b.angle,
            roundness: b.roundness,
            hardness: b.hardness,
            flip_x: b.flip_x,
            flip_y: b.flip_y,
            size: if is_plain(b) { b.size } else { 0.0 },
        }
    }
}

/// A round tip with no angle or flattening: the circle says it all.
fn is_plain(b: &BrushSettings) -> bool {
    matches!(b.tip, TipShape::Round) && b.angle == 0.0 && b.roundness >= 1.0
}

/// The size and kind of the aliased cursor tip the `tool` has: the Pencil's, the Eraser's in Pencil mode, and the
/// Eraser's square Block (`true`), which takes the block size.
fn aliased(app: &PhotocraftApp, tool: Tool) -> Option<(f32, bool)> {
    if crate::eraser_ui::block_mode(app, tool) {
        return Some((crate::eraser_ui::block_size(app), true));
    }
    (tool == Tool::Pencil || crate::eraser_ui::pencil_mode(app, tool)).then_some((app.session.tools.brush.size, false))
}

/// Builds the held tip for the active brush when its outline changed; a size change alone only updates the
/// diameter. Runs once per pass before the tool host, so [`cursor`] only reads.
pub(crate) fn sync_tip(app: &mut PhotocraftApp) {
    if aliased(app, app.ui.tool).is_some() {
        return;
    }
    let brush = &app.session.tools.brush;
    let key = TipKey::of(brush);
    let size = f64::from(brush.size);
    if let Some((held, tip)) = &mut app.brush_tip
        && *held == key
    {
        tip.diameter = size;
        return;
    }
    let tip = build(brush);
    app.brush_tip = Some((key, tip));
}

/// The tip of `brush`: two circles for a plain round tip, else the engine's traced outlines.
fn build(brush: &BrushSettings) -> BrushTip {
    let size = f64::from(brush.size);
    if !is_plain(brush)
        && let (Some(normal), Some(full)) = (photocraft_paint::outline::tip_outline(brush, false), photocraft_paint::outline::tip_outline(brush, true))
    {
        return BrushTip::new(size, BrushTipOutline::traced(TipRings::new(normal), TipRings::new(full)));
    }
    let normal = photocraft_paint::half_coverage_radius(brush.size / 2.0, brush.hardness).max(1.0);
    BrushTip::round(size, f64::from(normal))
}

/// The tip's axes for the pen's pose, in document space before the view: the brush angle from the shape dynamics
/// (pen tilt or barrel rotation), plus the barrel rotation and the tilt's shortening under Brush Projection. Upright
/// without shape dynamics or a pen.
pub(crate) fn pose(brush: &BrushSettings, pen: Option<PointerSample>) -> TipPose {
    let sd = &brush.shape_dynamics;
    let Some(pen) = pen.filter(|_| sd.enabled) else { return TipPose::upright() };
    let pen = pen.sanitized();
    let point = apply_pose(
        brush,
        photocraft_paint::StrokePoint {
            pressure: pen.pressure.clamp(0.0, 1.0),
            tilt_x: pen.tilt[0].to_degrees(),
            tilt_y: pen.tilt[1].to_degrees(),
            rotation: pen.rotation.to_degrees(),
            ..Default::default()
        },
    );
    let step = StepInput { point, direction: 0.0, initial_direction: 0.0, step: 0 };
    let mut angle = match sd.angle.control {
        Control::PenTilt | Control::Rotation => control_angle(sd.angle.control, sd.angle.fade_steps, &step),
        _ => 0.0,
    };
    if sd.brush_projection {
        angle += point.rotation;
    }
    TipPose::pen(f64::from(angle), sd.brush_projection.then_some([point.tilt_x, point.tilt_y]))
}

/// Preferences › Cursors › Painting Cursors as the framework names it.
fn painting_cursor(pref: PaintingPref) -> PaintingCursor {
    match pref {
        PaintingPref::Standard => PaintingCursor::Standard,
        PaintingPref::Precise => PaintingCursor::Precise,
        PaintingPref::NormalTip => PaintingCursor::NormalBrushTip,
        PaintingPref::FullSizeTip => PaintingCursor::FullSizeBrushTip,
    }
}

/// The cursor of the active painting tool over the canvas. Reads the tip [`sync_tip`] built; with no view or no
/// tip it is the system arrow.
pub(crate) fn cursor(app: &PhotocraftApp, cx: &ToolContext<'_>) -> Cursor {
    let arrow = Cursor::System(CursorIcon::Default);
    let Some(space) = cx.space() else { return arrow };
    let tool = app.ui.tool;
    let prefs = &app.session.prefs().cursors;
    let options = BrushCursorOptions::new(painting_cursor(prefs.painting))
        .show_only_crosshair_while_painting(prefs.show_only_crosshair_while_painting)
        .caps_lock_precise(true);
    let option = cx.mods.alt || app.ui.shell.latched.alt;
    if crate::canvas::cursor_policy::source_target(tool, option, prefs.painting, cx.caps_lock) {
        return crate::canvas::cursor_policy::sample_target();
    }
    let centre = crate::canvas::cursor_policy::brush_tip_centre(tool, option, prefs.show_crosshair_in_brush_tip);
    let painting = crate::canvas::paint_tool::stroking(app).is_some();
    // The framework's Standard cursor is the tool's icon.
    let icon = crate::icons::tool_icon(tool);
    let brush = &app.session.tools.brush;
    // Follow Perspective over a plane: the dab as it will land there ([`sync_surface`]).
    let surface = app.surface_cursor.filter(|_| matches!(tool, Tool::Brush | Tool::Pencil));
    if let Some((size, block)) = aliased(app, tool) {
        // The Pencil's dab on a plane is foreshortened, so it is outlined as the dab, not as the pixel cell.
        if let (Some(shape), false) = (surface, block) {
            let tip = BrushTip::round(f64::from(size), f64::from(size) / 2.0);
            let pose = surface_pose(shape, size / 2.0);
            return BrushCursor::new(&tip).pose(pose).centre(centre).painting(painting).icon(icon).cursor(cx, &options, &space.view);
        }
        // The whole-pixel cell the dab fills at the pointer; without a pointer there is nothing to outline.
        let Some(pointer) = cx.pointer.as_ref() else { return arrow };
        let at = space.view.to_doc(pointer.pos);
        let [x0, y0, x1, y1] = photocraft_engine::paint::grid_square(at.x, at.y, size);

        let round = !block && crate::canvas::cursor_policy::pencil_tip_is_round(size);
        let tip = BrushTip::new(f64::from(size), BrushTipOutline::pixels(Rect::new(x0, y0, x1, y1), round));
        return BrushCursor::new(&tip).centre(centre).painting(painting).icon(icon).cursor(cx, &options, &space.view);
    }
    let Some((_, tip)) = &app.brush_tip else { return arrow };
    let pose = match surface {
        Some(shape) => surface_pose(shape, brush.size / 2.0),
        None => pose(brush, cx.pointer.as_ref().map(|p| p.sample)),
    };
    BrushCursor::new(tip).pose(pose).centre(centre).painting(painting).icon(icon).cursor(cx, &options, &space.view)
}

/// The dab Follow Perspective lays down at document point `doc` with the Brush or Pencil: its radius and
/// foreshortening, from the engine. `None` when Follow Perspective is off, for another tool, or off every plane (the
/// cursor then stays as it is).
pub(crate) fn surface_shape(app: &PhotocraftApp, tool: Tool, doc: [f64; 2]) -> Option<CursorShape> {
    let brush = &app.session.tools.brush;
    if !matches!(tool, Tool::Brush | Tool::Pencil) || !brush.surface.perspective {
        return None;
    }
    photocraft_engine::brush_cmds::brush_cursor_shape(&app.session, doc[0], doc[1], brush.size / 2.0)
        .filter(|c| c.radius.is_finite() && c.radius > 0.0 && c.proj_angle.is_finite() && c.proj_scale.is_finite())
}

/// Keeps `PhotocraftApp::surface_cursor` on the dab under the pointer, once per pass before the tool host, so
/// [`cursor`] and `ui.inspect` only read it. Only the tip outlines (Normal and Full Size) follow the surface; without
/// a pointer over the canvas there is none.
pub(crate) fn sync_surface(app: &mut PhotocraftApp, cx: &ToolContext<'_>) {
    let tip = matches!(app.session.prefs().cursors.painting, PaintingPref::NormalTip | PaintingPref::FullSizeTip);
    let at = cx.space().zip(cx.pointer.as_ref()).map(|(space, pointer)| space.view.to_doc(pointer.pos));
    app.surface_cursor = at.filter(|_| tip).and_then(|at| surface_shape(app, app.ui.tool, [at.x, at.y]));
}

/// The pose that draws `shape` with a tip of radius `base_radius`: scaled to the dab's radius and shortened by its
/// `proj_scale` (at least 0.05) along `proj_angle` (radians, counter-clockwise, y up: the renderer's convention).
pub(crate) fn surface_pose(shape: CursorShape, base_radius: f32) -> TipPose {
    let k = f64::from(shape.radius / base_radius);
    let k = if k.is_finite() && k > 0.0 { k } else { 1.0 };
    let scale = f64::from(shape.proj_scale.clamp(0.05, 1.0));
    let (sin, cos) = f64::from(shape.proj_angle).sin_cos();
    let along = Vec2::new(cos, -sin);
    let project = |v: Vec2| (v + along * (v.dot(along) * (scale - 1.0))) * k;
    TipPose::new(project(Vec2::new(1.0, 0.0)), project(Vec2::new(0.0, 1.0)))
}

/// Outlines of document `idx`'s stored perspective planes while Follow Perspective is on and the Brush or Pencil is
/// active: thin lines in the overlay colour inside the canvas body at `at`, so the user sees where dabs follow.
pub(crate) fn show_planes(app: &PhotocraftApp, ui: &mut egui::Ui, at: egui::Rect, idx: usize) {
    if !matches!(app.ui.tool, Tool::Brush | Tool::Pencil) || !app.session.tools.brush.surface.perspective {
        return;
    }
    let Some(st) = app.session.documents().get(idx) else { return };
    let saved = &st.doc.perspective;
    if !saved.valid_for(st.doc.size.width, st.doc.size.height) {
        return;
    }
    let paths: Vec<BezPath> = saved
        .planes
        .iter()
        .filter(|c| c.iter().flatten().all(|v| v.is_finite()))
        .map(|c| {
            let mut path = BezPath::new();
            path.move_to((c[0][0], c[0][1]));
            for p in &c[1..] {
                path.line_to((p[0], p[1]));
            }
            path.close_path();
            path
        })
        .collect();
    if !paths.is_empty() {
        SelectionOutline::solid(tl!("Perspective planes").to_owned(), &paths).show_at(ui, at);
    }
}

#[cfg(test)]
#[path = "../surface_brush_tests.rs"]
mod surface_brush_tests;
