/// A 6000×4000 (24 MP) `mode`/`depth` document: one opaque raster layer of a varied pattern,
/// plus `spots` spot channels (the first sparse: 200×40 patches on a 600×400 grid, about 3 %;
/// the second covering 80 % with a horizontal ramp).
fn bench_doc(mode: photocraft_color::ColorMode, depth: photocraft_color::SampleType, spots: usize) -> photocraft_doc::Document {
    use photocraft_color::{Color, ColorMode, PixelFormat};
    use photocraft_doc::{AlphaChannel, Document, Layer, Size};
    use photocraft_geom::Rect;
    let (w, h) = (6000i32, 4000i32);
    let mut d = Document::new("bench", Size::new(w as u32, h as u32), mode, depth);
    let fmt = d.pixel_format();
    let n = fmt.channels();
    let mut layer = Layer::raster("pixels", fmt);
    let band = 200;
    for y0 in (0..h).step_by(band as usize) {
        let mut vals = Vec::with_capacity((w * band) as usize * n);
        for y in y0..y0 + band {
            for x in 0..w {
                for c in 0..n {
                    let v = if fmt.alpha && c == n - 1 { 1.0 } else { ((x * 7 + y * 3 + c as i32 * 50) % 256) as f32 / 255.0 };
                    vals.push(v);
                }
            }
        }
        layer.surface_mut().unwrap().write_region(Rect::new(0, y0, w, y0 + band), &vals);
    }
    d.layers.push(layer);
    let gray = PixelFormat::new(ColorMode::Grayscale, depth, false);
    if spots >= 1 {
        let mut s = photocraft_raster::Surface::new(gray);
        for gy in (0..h).step_by(400) {
            for gx in (0..w).step_by(600) {
                s.fill_rect(Rect::new(gx + 100, gy + 100, gx + 300, gy + 140), &[1.0]);
            }
        }
        d.channels.push(AlphaChannel { spot: Some((Color::rgb(0.0, 0.6, 0.65), 0.0)), ..AlphaChannel::new("sparse", s) });
    }
    if spots >= 2 {
        let mut s = photocraft_raster::Surface::new(gray);
        let (x0, x1) = (w / 10, w - w / 10);
        let row: Vec<f32> = (x0..x1).map(|x| (x - x0) as f32 / (x1 - x0) as f32).collect();
        for y0 in (0..h).step_by(band as usize) {
            let vals: Vec<f32> = (0..band).flat_map(|_| row.iter().copied()).collect();
            s.write_region(Rect::new(x0, y0, x1, y0 + band), &vals);
        }
        d.channels.push(AlphaChannel { spot: Some((Color::rgb(0.9, 0.2, 0.5), 0.5)), ..AlphaChannel::new("dense", s) });
    }
    d
}

/// Median wall time of `runs` calls of `f`, in ms.
fn median_ms(runs: usize, mut f: impl FnMut()) -> f64 {
    let mut t: Vec<f64> = (0..runs)
        .map(|_| {
            let s = std::time::Instant::now();
            f();
            s.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    t.sort_by(|a, b| a.total_cmp(b));
    t[t.len() / 2]
}

const BENCH_MODES: [photocraft_color::ColorMode; 2] = [photocraft_color::ColorMode::Rgb, photocraft_color::ColorMode::Cmyk];
const BENCH_DEPTHS: [photocraft_color::SampleType; 2] = [photocraft_color::SampleType::U8, photocraft_color::SampleType::U16];
