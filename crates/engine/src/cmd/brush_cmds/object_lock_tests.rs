//! Stay on Object through the commands and the live preview.

use super::*;
use crate::ai_layer::{Segmented, Segmenter};
use craft_ai_core::segment::{Click, Found};
use std::sync::{Arc, Mutex};

const W: u32 = 200;
const H: u32 = 100;
/// Two objects of one colour that touch at x = 100.
const A: Rect = Rect { x0: 20, y0: 10, x1: 100, y1: 90 };
const B: Rect = Rect { x0: 100, y0: 10, x1: 180, y1: 90 };
const RED: [f32; 4] = [0.8, 0.2, 0.2, 1.0];

/// A transparent document whose first layer has `paint(x, y)` where it returns a colour, with an
/// empty layer on top, active.
fn scene(paint: impl Fn(i32, i32) -> Option<[f32; 4]>) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": W, "height": H, "background": "transparent"})).unwrap();
    s.edit("photo", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        for y in 0..H as i32 {
            for x in 0..W as i32 {
                if let Some(c) = paint(x, y) {
                    surf.fill_rect(Rect::new(x, y, x + 1, y + 1), &c);
                }
            }
        }
        Ok(())
    })
    .unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s
}

fn two_objects() -> Session {
    scene(|x, y| (A.contains(x, y) || B.contains(x, y)).then_some(RED))
}

fn gray_png(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> Vec<u8> {
    let data: Vec<u8> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| f(x, y)).collect();
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&data).unwrap();
    out
}

type Calls = Arc<Mutex<Vec<(u32, u32, Vec<Click>)>>>;

/// A stand-in Select Subject that answers with whichever of `objects` (canvas pixels) is under the
/// click, and "nothing there" elsewhere; it records what it was asked.
fn segmenter(objects: Vec<Rect>) -> (Segmenter, Calls) {
    let calls: Calls = Arc::default();
    let seen = calls.clone();
    let seg: Segmenter = Arc::new(move |_png, w, h, clicks| {
        seen.lock().unwrap().push((w, h, clicks.to_vec()));
        let c = clicks.first().ok_or("no click")?;
        let (px, py) = ((c.x * w as f32) as i32, (c.y * h as f32) as i32);
        match objects.iter().find(|r| r.contains(px, py)) {
            Some(r) => Ok(Segmented {
                mask: gray_png(w, h, |x, y| if r.contains(x as i32, y as i32) { 255 } else { 0 }),
                found: Found { matched: true, score: Some(0.9), name: None },
            }),
            None => Ok(Segmented { mask: gray_png(w, h, |_, _| 0), found: Found { matched: false, score: None, name: None } }),
        }
    });
    (seg, calls)
}

fn layer_surface(s: &Session) -> Surface {
    let d = s.active().unwrap();
    d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().clone()
}

fn painted(surf: &Surface, x: i32, y: i32) -> bool {
    surf.rgba(x, y)[3] > 0.5
}

fn stroke(points: Value, surface: Value) -> Value {
    json!({"points": points, "color": "#0000ff", "target": "pixels", "brush": {"size": 20, "hardness": 1, "surface": surface}})
}

fn stay(extra: Value) -> Value {
    let mut o = json!({"stayOn": "object", "objectFeather": 0});
    for (k, v) in extra.as_object().unwrap() {
        o[k] = v.clone();
    }
    o
}

const ACROSS: [[i32; 2]; 2] = [[60, 50], [150, 50]];

#[test]
fn a_stroke_started_on_one_object_leaves_the_touching_one_alone() {
    let mut s = two_objects();
    let (seg, calls) = segmenter(vec![A, B]);
    s.ai.segmenter = Some(seg);
    s.execute("paint.stroke", stroke(json!(ACROSS), stay(json!({})))).unwrap();
    let paint = layer_surface(&s);
    assert!(painted(&paint, 60, 50) && painted(&paint, 98, 50), "the object under the first dab is painted");
    assert!(!painted(&paint, 101, 50) && !painted(&paint, 130, 50), "the touching object is not");
    assert!(!painted(&paint, 60, 5), "nor is anything outside the object");
    // One positive click at the first dab, as fractions of the picture the tool saw.
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let (w, h, clicks) = &calls[0];
    assert_eq!((*w, *h), (W, H));
    assert_eq!(clicks.len(), 1);
    assert!(clicks[0].positive && (clicks[0].x - 0.3).abs() < 0.01 && (clicks[0].y - 0.5).abs() < 0.01, "{clicks:?}");
    drop(calls);
    // Started on the other object, the stroke stays on that one.
    let mut other = two_objects();
    other.ai.segmenter = Some(segmenter(vec![A, B]).0);
    other.execute("paint.stroke", stroke(json!([[150, 50], [60, 50]]), stay(json!({})))).unwrap();
    let paint = layer_surface(&other);
    assert!(painted(&paint, 150, 50) && !painted(&paint, 60, 50) && !painted(&paint, 98, 50));
    // Off paints across both objects.
    let mut off = two_objects();
    off.execute("paint.stroke", stroke(json!(ACROSS), json!({}))).unwrap();
    let o = layer_surface(&off);
    assert!(painted(&o, 60, 50) && painted(&o, 130, 50));
}

#[test]
fn a_live_stroke_stays_on_its_object_and_commits_what_it_shows() {
    let p = stroke(json!([[60, 50]]), stay(json!({})));
    let mut s = two_objects();
    s.ai.segmenter = Some(segmenter(vec![A, B]).0);
    let mut live = LiveStroke::begin_with(&s, "paint.stroke", &p).unwrap();
    // Nothing is found until a push: starting a stroke never waits for the finder.
    assert!(live.is_pending());
    assert_eq!(s.surface_cache.object_finds(), 0);
    live.push(&[]).unwrap();
    assert!(!live.is_pending() && s.surface_cache.object_finds() == 1);
    live.push(&[StrokePoint::new(100.0, 50.0, 1.0), StrokePoint::new(150.0, 50.0, 1.0)]).unwrap();
    let id = s.active().unwrap().active_layer.unwrap();
    let shown = live.doc.layer(id).unwrap().surface().unwrap().clone();
    assert!(painted(&shown, 60, 50) && painted(&shown, 98, 50), "the first point was painted once the object was known");
    assert!(!painted(&shown, 101, 50) && !painted(&shown, 130, 50));
    let mut commit = p.clone();
    commit["points"] = json!([[60, 50], [100, 50], [150, 50]]);
    commit["seed"] = json!(live.seed);
    s.offer_live_stroke(live).unwrap();
    let r = s.execute("paint.stroke", commit.clone()).unwrap();
    s.clear_live_stroke();
    assert_eq!(r["fromLive"], true);
    let area = Rect::new(0, 0, W as i32, H as i32);
    let same = |a: &Surface, b: &Surface| (area.y0..area.y1).all(|y| (area.x0..area.x1).all(|x| a.pixel(x, y) == b.pixel(x, y)));
    assert!(same(&shown, &layer_surface(&s)), "the preview shows what commits");
    // A replay without the live stroke renders the same pixels.
    let mut fresh = two_objects();
    fresh.ai.segmenter = Some(segmenter(vec![A, B]).0);
    fresh.execute("paint.stroke", commit).unwrap();
    assert!(same(&layer_surface(&fresh), &layer_surface(&s)));
    // A live stroke pushed all at once (no empty push) works too, and one that starts on B stays on B.
    let mut s2 = two_objects();
    s2.ai.segmenter = Some(segmenter(vec![A, B]).0);
    let mut live = LiveStroke::begin_with(&s2, "paint.stroke", &stroke(json!([[150, 50]]), stay(json!({})))).unwrap();
    live.push(&[StrokePoint::new(60.0, 50.0, 1.0)]).unwrap();
    let id = s2.active().unwrap().active_layer.unwrap();
    let shown = live.doc.layer(id).unwrap().surface().unwrap().clone();
    assert!(painted(&shown, 150, 50) && !painted(&shown, 60, 50));
}

#[test]
fn nothing_under_the_brush_is_an_error_and_changes_nothing() {
    let mut s = two_objects();
    s.ai.segmenter = Some(segmenter(vec![A]).0);
    let p = stroke(json!([[190, 95], [150, 50]]), stay(json!({})));
    let e = s.execute("paint.stroke", p.clone()).unwrap_err().to_string();
    assert!(e.contains("no object found under the brush"), "{e}");
    assert!(layer_surface(&s).content_bounds().is_empty());
    // The live stroke says it on its first step.
    let mut live = LiveStroke::begin_with(&s, "paint.stroke", &p).unwrap();
    assert!(live.push(&[]).unwrap_err().to_string().contains("no object found"));
    // A first dab off the canvas is the same error, not a panic.
    let e = s.execute("paint.stroke", stroke(json!([[-30, 50], [60, 50]]), stay(json!({})))).unwrap_err().to_string();
    assert!(e.contains("no object found"), "{e}");
    // A failing tool is an error too, not a quiet switch to the built-in finder.
    s.ai.segmenter = Some(Arc::new(|_, _, _, _| Err("the model broke".to_owned())));
    let e = s.execute("paint.stroke", stroke(json!(ACROSS), stay(json!({})))).unwrap_err().to_string();
    assert!(e.contains("the model broke"), "{e}");
    assert!(layer_surface(&s).content_bounds().is_empty());
}

#[test]
fn the_built_in_finder_is_used_without_the_tool() {
    // A red disc on a green field; no host and no stand-in, so the Object Finder runs.
    let disc = |x: i32, y: i32| (x - 100).pow(2) + (y - 50).pow(2) <= 30 * 30;
    let mut s = scene(|x, y| Some(if disc(x, y) { [0.85, 0.1, 0.1, 1.0] } else { [0.1, 0.6, 0.15, 1.0] }));
    assert_eq!(finder(&s), Finder::BuiltIn);
    s.execute("paint.stroke", stroke(json!([[100, 50], [195, 50]]), stay(json!({})))).unwrap();
    let paint = layer_surface(&s);
    assert!(painted(&paint, 100, 50) && painted(&paint, 120, 50), "the disc is painted");
    assert!(!painted(&paint, 160, 50) && !painted(&paint, 190, 50) && !painted(&paint, 10, 50), "the field is not");
    assert_eq!(s.surface_cache.object_finds(), 1);
}

#[test]
fn which_finder_runs_follows_the_tool_status() {
    let mut s = Session::new();
    assert_eq!(finder(&s), Finder::BuiltIn);
    assert!(ai_status(&s).is_some());
    s.ai.segmenter = Some(segmenter(vec![]).0);
    assert_eq!((finder(&s), ai_status(&s)), (Finder::Ai, None));
    // A host with nothing set up still means the built-in finder, and says why.
    let empty = Session::new();
    empty.ai.set_host(crate::ai_layer::AiHost::new(craft_ai_native::shared::SharedStore::new("/nonexistent/x.json"), || Err("not set up".to_owned())));
    assert_eq!(empty.ai.status_reason_now(crate::ai_layer::SELECT_SUBJECT_TOOL).as_deref(), Some("not set up"));
    assert_eq!(finder(&empty), Finder::BuiltIn);
    assert_eq!(ai_status(&empty).as_deref(), Some("not set up"));
}

#[test]
fn the_feathered_edge_fades_the_paint_out() {
    let mut s = two_objects();
    s.ai.segmenter = Some(segmenter(vec![A, B]).0);
    s.execute("paint.stroke", stroke(json!([[60, 50], [98, 50]]), stay(json!({"objectFeather": 6})))).unwrap();
    let paint = layer_surface(&s);
    let alpha: Vec<f32> = (70..110).map(|x| paint.rgba(x, 50)[3]).collect();
    assert!(alpha.iter().all(|a| (0.0..=1.0).contains(a)));
    // It falls as the edge at 100 is crossed, without a jump, and is soft rather than cut.
    assert!(alpha[..20].windows(2).all(|w| w[0] + 0.02 >= w[1] || w[0] > 0.97), "{alpha:?}");
    let (inside, edge, outside) = (paint.rgba(75, 50)[3], paint.rgba(99, 50)[3], paint.rgba(112, 50)[3]);
    assert!(inside > 0.95 && edge > 0.2 && edge < 0.8 && outside < 0.05, "{inside} {edge} {outside}");
}

#[test]
fn the_object_is_cached_while_the_sampled_pixels_stay_the_same() {
    let mut s = two_objects();
    s.ai.segmenter = Some(segmenter(vec![A, B]).0);
    s.execute("paint.stroke", stroke(json!([[40, 40], [80, 60]]), stay(json!({})))).unwrap();
    assert_eq!(s.surface_cache.object_finds(), 1);
    // Painting the target layer does not change an All Layers sample, so the next stroke on the
    // object finds it again at once.
    s.execute("paint.stroke", stroke(json!([[50, 70], [90, 30]]), stay(json!({})))).unwrap();
    assert_eq!(s.surface_cache.object_finds(), 1, "the second stroke reused the object");
    // The other object is another find.
    s.execute("paint.stroke", stroke(json!([[140, 50], [160, 50]]), stay(json!({})))).unwrap();
    assert_eq!(s.surface_cache.object_finds(), 2);
    // Editing a layer the sample reads invalidates it.
    let photo = s.active().unwrap().doc.layers[0].id;
    s.edit("touch the photo", |doc, _| {
        doc.layer_mut(photo).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, 4, 4), &[0.0, 1.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    s.execute("paint.stroke", stroke(json!([[40, 40], [80, 60]]), stay(json!({})))).unwrap();
    assert_eq!(s.surface_cache.object_finds(), 3, "a changed photo is found again");
}

#[test]
fn the_selection_still_applies_and_a_disjoint_one_paints_nothing() {
    let mut s = two_objects();
    s.ai.segmenter = Some(segmenter(vec![A, B]).0);
    // A selection of the lower half of object A: the stroke paints the part of the object inside it.
    s.execute("select.rect", json!({"x": 0, "y": 50, "width": 100, "height": 50})).unwrap();
    s.execute("paint.stroke", stroke(json!([[60, 70], [95, 70], [95, 20], [40, 20]]), stay(json!({})))).unwrap();
    let paint = layer_surface(&s);
    assert!(painted(&paint, 60, 70) && !painted(&paint, 95, 20) && !painted(&paint, 40, 20), "inside the selection and the object only");
    // A selection that misses the object: nothing is painted (and not everything).
    let mut far = two_objects();
    far.ai.segmenter = Some(segmenter(vec![A, B]).0);
    far.execute("select.rect", json!({"x": 110, "y": 0, "width": 80, "height": 100})).unwrap();
    far.execute("paint.stroke", stroke(json!(ACROSS), stay(json!({})))).unwrap();
    assert!(layer_surface(&far).content_bounds().is_empty());
}

#[test]
fn depth_tells_apart_two_objects_of_one_colour() {
    // One segment covers both (they are one colour); the depth is far on the left, near on the right.
    let mut s = two_objects();
    let both = Rect::new(20, 10, 180, 90);
    s.ai.segmenter = Some(segmenter(vec![both]).0);
    s.edit("depth", |doc, _| {
        let fmt = photocraft_color::PixelFormat::new(photocraft_color::ColorMode::Grayscale, doc.depth, false);
        let mut surf = Surface::new(fmt);
        let data: Vec<f32> = (0..H as i32).flat_map(|_| (0..W as i32).map(|x| if x < 100 { 0.8 } else { 0.3 })).collect();
        surf.write_region(Rect::new(0, 0, W as i32, H as i32), &data);
        doc.channels.push(photocraft_doc::AlphaChannel::new("Depth".to_string(), surf));
        Ok(())
    })
    .unwrap();
    let both_depths = stay(json!({"stayOn": "objectAndDepth", "depthTolerance": 0.1}));
    s.execute("paint.stroke", stroke(json!(ACROSS), both_depths.clone())).unwrap();
    let paint = layer_surface(&s);
    assert!(painted(&paint, 60, 50) && painted(&paint, 98, 50));
    assert!(!painted(&paint, 105, 50) && !painted(&paint, 130, 50), "the nearer object is not painted");
    // Object alone paints both, the same segment.
    let mut plain = two_objects();
    plain.ai.segmenter = Some(segmenter(vec![both]).0);
    plain.execute("paint.stroke", stroke(json!(ACROSS), stay(json!({})))).unwrap();
    assert!(painted(&layer_surface(&plain), 60, 50) && painted(&layer_surface(&plain), 130, 50));
    // A wide tolerance lets the depths join again.
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("paint.stroke", stroke(json!(ACROSS), stay(json!({"stayOn": "objectAndDepth", "depthTolerance": 0.6})))).unwrap();
    assert!(painted(&layer_surface(&s), 130, 50));
    // Without the channel the stroke says what to do, and changes nothing.
    let mut none = two_objects();
    none.ai.segmenter = Some(segmenter(vec![both]).0);
    let e = none.execute("paint.stroke", stroke(json!(ACROSS), both_depths)).unwrap_err().to_string();
    assert!(e.contains("Estimate Depth"), "{e}");
    assert!(layer_surface(&none).content_bounds().is_empty());
}

#[test]
fn bad_options_are_errors() {
    let mut s = two_objects();
    s.ai.segmenter = Some(segmenter(vec![A, B]).0);
    for bad in [
        json!({"stayOn": "everything"}),
        json!({"stayOn": "object", "objectFeather": 65}),
        json!({"stayOn": "object", "objectFeather": -1}),
        json!({"stayOn": "object", "depthTolerance": 1.5}),
        json!({"stayOn": "object", "depthTolerance": -0.1}),
    ] {
        assert!(s.execute("paint.stroke", stroke(json!(ACROSS), bad.clone())).is_err(), "{bad}");
        assert!(s.execute("tools.setBrush", json!({"surface": bad.clone()})).is_err(), "{bad}");
    }
    assert!(layer_surface(&s).content_bounds().is_empty());
    // The options are kept in the brush, and the Eraser ignores them.
    s.execute("tools.setBrush", json!({"surface": {"stayOn": "objectAndDepth", "objectFeather": 3, "depthTolerance": 0.2}})).unwrap();
    let o = &s.tools.brush.surface;
    assert_eq!((o.stay_on, o.object_feather, o.depth_tolerance), (photocraft_paint::StayOn::ObjectAndDepth, 3.0, 0.2));
    s.execute("tools.setBrush", json!({"surface": {"stayOn": "off"}})).unwrap();
    let mut erase = two_objects();
    erase.ai.segmenter = Some(Arc::new(|_, _, _, _| Err("never asked".to_owned())));
    erase.execute("paint.stroke", json!({"points": ACROSS, "erase": true, "target": "pixels", "brush": {"size": 20, "surface": stay(json!({}))}})).unwrap();
}

#[test]
fn cancelling_a_pending_find_ends_the_wait_at_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut s = two_objects();
    // A segmenter that does not answer until released.
    let release = Arc::new(AtomicBool::new(false));
    let gate = release.clone();
    s.ai.segmenter = Some(Arc::new(move |_, _, _, _| {
        while !gate.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Err("released".to_owned())
    }));
    let mut live = LiveStroke::begin_with(&s, "paint.stroke", &stroke(json!([[60, 50]]), stay(json!({})))).unwrap();
    let cancel = live.cancel_pending().expect("Stay on Object has a pending find to cancel");
    let waiting = std::thread::spawn(move || live.push(&[]));
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(!waiting.is_finished(), "the push waits for the finder");
    let started = std::time::Instant::now();
    cancel.store(true, Ordering::SeqCst);
    let result = waiting.join().unwrap();
    assert!(matches!(result, Err(EngineError::Cancelled)), "{result:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "the wait ended without the finder answering");
    release.store(true, Ordering::SeqCst);
    // Without Stay on Object there is nothing to cancel.
    let plain = LiveStroke::begin_with(&s, "paint.stroke", &stroke(json!([[60, 50]]), json!({}))).unwrap();
    assert!(plain.cancel_pending().is_none());
}
