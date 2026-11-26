//! A stroke composited tile by tile while it is drawn (`StrokeRenderer::settle`) ends as the
//! pixels one composite of the whole stroke writes.

use super::*;
use serde_json::json;

const W: i32 = 800;
const H: i32 = 140;

fn brush(extra: serde_json::Value) -> BrushSettings {
    let mut b = json!({"size": 36, "hardness": 0.4, "flow": 0.6, "opacity": 0.8, "color": [0.8, 0.1, 0.3, 1.0], "seed": 5});
    for (k, v) in extra.as_object().unwrap() {
        b[k] = v.clone();
    }
    serde_json::from_value(b).unwrap()
}

fn pre() -> Surface {
    let mut s = Surface::with_default(PixelFormat::RGBA8, &[0.9, 0.85, 0.7, 1.0]);
    let r = Rect::new(20, 10, 260, 110);
    let data: Vec<f32> = (r.y0..r.y1)
        .flat_map(|y| {
            (r.x0..r.x1).flat_map(move |x| [((x * 7 + y * 3) % 31) as f32 / 30.0, 0.5, ((x + y) % 11) as f32 / 10.0, if (x + y) % 6 == 0 { 0.4 } else { 1.0 }])
        })
        .collect();
    s.write_region(r, &data);
    s
}

fn path() -> Vec<StrokePoint> {
    (0..80).map(|i| StrokePoint::new(30.0 + 9.4 * f64::from(i), 70.0 + 30.0 * (f64::from(i) / 7.0).sin(), 0.4 + 0.6 * ((i * 5) % 7) as f32 / 6.0)).collect()
}

fn same(a: &Surface, b: &Surface) -> bool {
    (0..H).all(|y| (0..W).all(|x| a.rgba(x, y).map(f32::to_bits) == b.rgba(x, y).map(f32::to_bits)))
}

/// Draws the stroke one point at a time, settling `max` tiles every `every` points, then compares
/// with one composite of the finished stroke. Returns how many tiles `settle` wrote mid-stroke.
fn check(name: &str, b: &BrushSettings, every: usize, max: usize, lock: bool, selection: Option<&Surface>) -> usize {
    let pre = pre();
    let mut shadow = pre.clone();
    let mut r = StrokeRenderer::new(b, Some(pre.format()), 1.0);
    let mut early = 0;
    for (i, p) in path().iter().enumerate() {
        r.push(std::slice::from_ref(p));
        if i % every == 0 {
            let batch = r.settle(&pre, &mut shadow, selection, lock, max, Rect::EMPTY);
            early += batch.keys.len();
            // A surface tile's coverage tiles go together, so the last one may pass the limit.
            assert!(batch.keys.len() < max + (SETTLE_GROUP * SETTLE_GROUP) as usize, "{name}: settled {} tiles for a limit of {max}", batch.keys.len());
        }
    }
    let mut whole = StrokeRenderer::new(b, Some(pre.format()), 1.0);
    whole.push(&path());
    whole.finish();
    let mut expected = pre.clone();
    whole.composite(&pre, &mut expected, selection, lock, true);
    r.finish();
    r.settle_all(&pre, &mut shadow, selection, lock);
    assert!(same(&shadow, &expected), "{name}: the settled stroke differs from one composite");
    assert!(r.unsettled_keys().is_empty(), "{name}: tiles left unsettled");
    early
}

#[test]
fn a_stroke_settled_along_the_way_equals_one_composite() {
    let mut sel = Surface::with_default(PixelFormat::GRAY8, &[0.0]);
    sel.fill_rect(Rect::new(60, 20, 250, 120), &[1.0]);
    let cases = [
        ("plain", brush(json!({}))),
        ("dual brush", brush(json!({"dualBrush": {"enabled": true, "size": 11, "spacing": 0.4}}))),
        ("wet edges", brush(json!({"wetEdges": true}))),
        ("texture", brush(json!({"texture": {"enabled": true, "depth": 0.7}}))),
        ("smoothing", brush(json!({"smoothing": {"amount": 0.6}}))),
        ("erase", brush(json!({"erase": true}))),
    ];
    for (name, b) in &cases {
        for (every, max) in [(1, 1), (3, 2), (7, 1000)] {
            let early = check(name, b, every, max, false, None);
            assert!(early > 0, "{name}: nothing was settled while drawing (every {every}, {max} at a time)");
        }
        check(name, b, 2, 3, true, Some(&sel));
    }
}

/// Pushes a point at (`x`, 60) and settles, giving the tiles settled.
fn push_and_settle(r: &mut StrokeRenderer, pre: &Surface, shadow: &mut Surface, x: f64, exclude: Rect) -> Vec<(i32, i32)> {
    r.push(&[StrokePoint::new(x, 60.0, 1.0)]);
    r.settle(pre, shadow, None, false, 1000, exclude).keys
}

#[test]
fn settle_waits_for_a_tile_to_be_calm_and_takes_its_whole_surface_tile() {
    let pre = pre();
    let mut shadow = pre.clone();
    let mut r = StrokeRenderer::new(&brush(json!({})), Some(pre.format()), 1.0);
    // A stroke in the first surface tile, then the pen moves on into the third.
    r.push(&[StrokePoint::new(60.0, 60.0, 1.0), StrokePoint::new(200.0, 60.0, 1.0)]);
    assert!(r.settle(&pre, &mut shadow, None, false, 1000, Rect::EMPTY).keys.is_empty(), "everything was touched by the only push");
    let mut pushes = 0;
    let settled = loop {
        pushes += 1;
        assert!(pushes <= CALM_PUSHES + 1, "the first tile never settled");
        let keys = push_and_settle(&mut r, &pre, &mut shadow, 700.0 + f64::from(pushes as i32), Rect::EMPTY);
        if !keys.is_empty() {
            break keys;
        }
    };
    assert_eq!(pushes, CALM_PUSHES, "a tile settles once the latest pushes left it alone, not before");
    assert!(settled.iter().all(|k| k.0.div_euclid(SETTLE_GROUP) == 0), "only the first surface tile's coverage settled");
    let left = r.unsettled_keys();
    assert!(left.iter().all(|k| k.0.div_euclid(SETTLE_GROUP) != 0), "a surface tile settles whole");
    assert!(!same(&shadow, &pre), "the first tiles settled");
    assert_eq!(shadow.rgba(700, 60), pre.rgba(700, 60), "the head's did not");
}

#[test]
fn a_tile_that_is_still_changing_keeps_its_surface_tile_back() {
    let pre = pre();
    let mut shadow = pre.clone();
    let mut r = StrokeRenderer::new(&brush(json!({})), Some(pre.format()), 1.0);
    r.push(&[StrokePoint::new(60.0, 60.0, 1.0)]);
    // The pen stays in the same surface tile, a coverage tile or two on.
    for i in 0..=CALM_PUSHES + 1 {
        let keys = push_and_settle(&mut r, &pre, &mut shadow, 200.0 + f64::from(i as i32), Rect::EMPTY);
        assert!(keys.is_empty(), "the surface tile still changes");
    }
    assert!(same(&shadow, &pre));
    // The tile the tail covers is kept back too, however calm.
    for i in 0..=CALM_PUSHES + 1 {
        let keys = push_and_settle(&mut r, &pre, &mut shadow, 700.0 + f64::from(i as i32), Rect::new(0, 0, 256, 140));
        assert!(keys.is_empty(), "an excluded tile settled");
    }
    assert!(same(&shadow, &pre));
    let keys = push_and_settle(&mut r, &pre, &mut shadow, 710.0, Rect::EMPTY);
    assert!(!keys.is_empty() && !same(&shadow, &pre), "it settles once nothing keeps it back");
}
