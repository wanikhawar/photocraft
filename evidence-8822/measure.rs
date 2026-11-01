
#[cfg(test)]
#[test]
#[ignore]
fn issue_8822_measurement() {
    use std::{collections::HashSet, sync::Arc, time::Instant};
    let case = std::env::var("REFINE_CASE").unwrap();
    let implementation = std::env::var("REFINE_IMPL").unwrap();
    let large = case.ends_with("100");
    let canvas = if large { Rect::new(0, 0, 100_000, 100_000) } else { Rect::new(0, 0, 6000, 4000) };
    let mut mask = Surface::new(PixelFormat::GRAY8);
    if case.starts_with("solid") { mask.fill_rect(canvas, &[1.0]); }
    else if case.starts_with("small") { mask.fill_rect(Rect::new(40, 50, 160, 170), &[1.0]); }
    else { mask = brush_surface(&[(1.0, 1.0), ((canvas.x1 - 2) as f32, (canvas.y1 - 2) as f32)], 3.0, 100.0, canvas).unwrap(); }
    let source = Surface::with_default(PixelFormat::RGBA8, &[0.5, 0.3, 0.7, 1.0]);
    let sampler = crate::segment::SurfaceSampler(&source);
    let p = RefineParams { radius: 4.0, feather: 1.0, ..Default::default() };
    let start = Instant::now();
    let result = if implementation == "dense" {
        assert!(!large, "never execute dense huge reference");
        let r = refine_mask(&sampler, &surface_reader(&mask), mask.content_bounds(), canvas, &p).unwrap();
        region_surface(&r)
    } else { refine_surface(&sampler, &mask, canvas, &p, None).unwrap().unwrap() };
    let elapsed = start.elapsed();
    use std::hash::{Hash, Hasher};
    let mut hashes = std::collections::HashMap::new();
    let mut checksum = std::collections::hash_map::DefaultHasher::new();
    for (tc, tile) in result.tiles() {
        let key = Arc::as_ptr(tile) as usize;
        let hash = hashes.entry(key).or_insert_with(|| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            tile.bytes().hash(&mut h); h.finish()
        });
        tc.hash(&mut checksum); hash.hash(&mut checksum);
    }
    println!("CHECKSUM impl={implementation} case={case} batch={} value={:016x}", std::env::var("REFINE_BATCH").unwrap_or_default(), checksum.finish());
    let mut seen = HashSet::new();
    let unique_bytes: usize = result.tiles().filter(|(_, t)| seen.insert(Arc::as_ptr(t) as usize)).map(|(_, t)| t.bytes().len()).sum();
    println!("MEASUREMENT impl={implementation} case={case} batch={} ms={:.3} tiles={} unique_pixel_bytes={unique_bytes}", std::env::var("REFINE_BATCH").unwrap_or_default(), elapsed.as_secs_f64() * 1000.0, result.tile_count());
}
