//! A stroke composited tile by tile while it is drawn (`StrokeRenderer::settle`) ends as the
//! pixels one composite of the whole stroke writes.

use super::*;
use serde_json::json;

const W: i32 = 320;
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
    (0..48).map(|i| StrokePoint::new(30.0 + 5.6 * f64::from(i), 70.0 + 30.0 * (f64::from(i) / 7.0).sin(), 0.4 + 0.6 * ((i * 5) % 7) as f32 / 6.0)).collect()
}

/// Out to the right and back over the same tiles, so tiles that settled change again.
fn looping_path() -> Vec<StrokePoint> {
    let out = (0..30).map(|i| StrokePoint::new(30.0 + 8.0 * f64::from(i), 50.0 + 10.0 * (f64::from(i) / 5.0).sin(), 0.7));
    let back = (0..30).map(|i| StrokePoint::new(270.0 - 8.0 * f64::from(i), 62.0 + 10.0 * (f64::from(i) / 4.0).cos(), 1.0));
    out.chain(back).collect()
}

fn same(a: &Surface, b: &Surface) -> bool {
    (0..H).all(|y| (0..W).all(|x| a.rgba(x, y).map(f32::to_bits) == b.rgba(x, y).map(f32::to_bits)))
}

/// Draws `pts` one point at a time, settling up to `max` tiles that were calm for `calm` pushes
/// every `every` points, then compares with one composite of the finished stroke. Returns how many
/// tiles `settle` wrote mid-stroke. A selection is used with the transparency lock on.
fn check(name: &str, b: &BrushSettings, pts: &[StrokePoint], every: usize, calm: u64, max: usize, selection: Option<&Surface>) -> usize {
    // A selection comes with the transparency lock, so both inputs are covered.
    let lock = selection.is_some();
    let pre = pre();
    let mut shadow = pre.clone();
    let mut r = StrokeRenderer::new(b, Some(pre.format()), 1.0);
    r.track_settling();
    let mut early = 0;
    for (i, p) in pts.iter().enumerate() {
        r.push(std::slice::from_ref(p));
        if i % every == 0 {
            let n = r.settle(&pre, &mut shadow, selection, lock, calm, max);
            assert!(n <= max, "{name}: settled {n} tiles for a limit of {max}");
            early += n;
        }
    }
    let mut whole = StrokeRenderer::new(b, Some(pre.format()), 1.0);
    whole.push(pts);
    whole.finish();
    let mut expected = pre.clone();
    whole.composite(&pre, &mut expected, selection, lock, true);
    r.finish();
    r.settle_all(&pre, &mut shadow, selection, lock);
    assert!(same(&shadow, &expected), "{name}: the settled stroke differs from one composite");
    assert_eq!(r.unsettled_count(), 0, "{name}: tiles left unsettled");
    early
}

#[test]
fn a_stroke_settled_along_the_way_equals_one_composite() {
    let mut sel = Surface::with_default(PixelFormat::GRAY8, &[0.0]);
    sel.fill_rect(Rect::new(60, 20, 250, 120), &[1.0]);
    let everything = brush(json!({
        "dualBrush": {"enabled": true, "size": 11, "spacing": 0.4},
        "wetEdges": true,
        "texture": {"enabled": true, "depth": 0.7}
    }));
    let cases = [
        ("plain", brush(json!({}))),
        ("dual brush", brush(json!({"dualBrush": {"enabled": true, "size": 11, "spacing": 0.4}}))),
        ("wet edges", brush(json!({"wetEdges": true}))),
        ("texture", brush(json!({"texture": {"enabled": true, "depth": 0.7}}))),
        ("smoothing", brush(json!({"smoothing": {"amount": 0.6}}))),
        ("erase", brush(json!({"erase": true}))),
        ("dual + wet + texture", everything.clone()),
    ];
    for (name, b) in &cases {
        for (every, max) in [(1, 1), (3, 2), (7, 1000)] {
            let early = check(name, b, &path(), every, 2, max, None);
            assert!(early > 0, "{name}: nothing was settled while drawing (every {every}, {max} at a time)");
        }
        check(name, b, &path(), 2, 2, 3, Some(&sel));
    }
    // A stroke that comes back over tiles that already settled changes them again.
    let early = check("loop", &everything, &looping_path(), 2, 2, 4, None);
    assert!(early > 0, "the looping stroke settled nothing");
    check("loop with selection", &everything, &looping_path(), 2, 2, 4, Some(&sel));
}

#[test]
fn settle_waits_for_a_tile_to_be_calm() {
    let pre = pre();
    let mut shadow = pre.clone();
    let b = brush(json!({}));
    let mut r = StrokeRenderer::new(&b, Some(pre.format()), 1.0);
    r.track_settling();
    r.push(&[StrokePoint::new(60.0, 60.0, 1.0), StrokePoint::new(110.0, 60.0, 1.0)]);
    assert_eq!(r.settle(&pre, &mut shadow, None, false, 2, 1000), 0, "the only push changed every tile just now");
    assert!(same(&shadow, &pre));
    // A far-away push: the first push's tiles have been calm for one push only.
    r.push(&[StrokePoint::new(240.0, 100.0, 1.0)]);
    assert_eq!(r.settle(&pre, &mut shadow, None, false, 2, 1000), 0, "calm for one push is not calm for two");
    r.push(&[StrokePoint::new(242.0, 100.0, 1.0)]);
    let n = r.settle(&pre, &mut shadow, None, false, 2, 1000);
    assert!(n > 0, "the first push's tiles have been calm for two pushes");
    assert!(!same(&shadow, &pre), "the calm tiles were composited");
    assert_eq!(shadow.rgba(240, 100), pre.rgba(240, 100), "the tiles under the moving brush were not");
    assert!(r.unsettled_count() > 0, "the tiles under the moving brush are still unsettled");
}
