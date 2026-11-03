//! Regression evidence that Photomerge's memory estimate and its band budget reflect real
//! allocations.
//!
//! [`allocation_counter::measure`] counts allocations made on the thread that runs the closure,
//! so each measurement below runs inside `rayon::ThreadPool::install` on a one-worker pool: the
//! closure and every Rayon task it spawns stay on that counted worker.
//!
//! The scope is deliberately narrow. The counters see only heap requests made while the closure
//! runs; they do not see the caller's documents, buffers already held by the process-wide tile
//! scratch pool (a freed tile is reused, not reallocated), other test threads, allocator
//! bookkeeping or the process's RSS. Buffers reused from the scratch pool are therefore missing
//! from the measured peak, which makes it a lower bound on the real peak: it can under-report,
//! never invent, memory. The estimates still have to cover it.

use super::*;
use allocation_counter::measure;
use rayon::ThreadPoolBuilder;
use std::hint::black_box;

/// Two overlapping images, their real content sizes and the canvas that contains them.
struct BlendFixture {
    fmt: PixelFormat,
    first: Rect,
    second: Rect,
    canvas: Rect,
}

/// A fixture whose writable areas are deliberately not tile-aligned.
fn fixture(fmt: PixelFormat, first: (usize, usize), second: (usize, usize), offset: (i32, i32)) -> BlendFixture {
    let first = Rect::new(0, 0, first.0 as i32, first.1 as i32);
    let second = Rect::new(offset.0, offset.1, offset.0 + second.0 as i32, offset.1 + second.1 as i32);
    let canvas = Rect::new(0, 0, first.x1.max(second.x1), first.y1.max(second.y1));
    BlendFixture { fmt, first, second, canvas }
}

/// An opaque, textured RGBA surface over `area`. Called inside the measured closure.
fn textured(fmt: PixelFormat, area: Rect, base: f32) -> Surface {
    let nch = fmt.channels();
    let w = area.width() as usize;
    let h = area.height().max(1) as f32;
    let mut s = Surface::new(fmt);
    let mut row = vec![0.0f32; w * nch];
    for y in area.y0..area.y1 {
        for x in 0..w {
            let t = (x as f32 / w.max(1) as f32 + (y - area.y0) as f32 / h) * 0.1;
            for c in 0..nch - 1 {
                row[x * nch + c] = base + t + 0.05 * c as f32;
            }
            row[x * nch + nch - 1] = 1.0;
        }
        s.write_region(Rect::new(area.x0, y, area.x1, y + 1), &row);
    }
    s
}

fn single_worker() -> std::result::Result<rayon::ThreadPool, rayon::ThreadPoolBuildError> {
    ThreadPoolBuilder::new().num_threads(1).build()
}

fn pyramid_levels(f: &BlendFixture) -> usize {
    panorama::blend_levels(f.canvas.width() as usize, f.canvas.height() as usize)
}

/// Blends the fixture with the warped surfaces created and destroyed inside the measured
/// closure. Returns `(count_total, bytes_max, succeeded)`.
fn measure_blend(f: &BlendFixture, pool: &rayon::ThreadPool) -> (u64, u64, bool) {
    let mut ok = false;
    let info = pool.install(|| {
        measure(|| {
            let warped = vec![textured(f.fmt, f.first, 0.25), textured(f.fmt, f.second, 0.55)];
            let opt = BlendOptions { seamless: true, gains: true, geometry: None, budget: None };
            let blended = seam_blend(warped, f.canvas, &[0, 1], opt, None);
            ok = blended.is_ok();
            if let Ok(b) = blended {
                black_box(&b.layers);
                black_box(&b.composite);
            }
        })
    });
    (info.count_total, info.bytes_max, ok)
}

#[test]
fn seam_blend_allocations_fit_the_estimate() {
    let u8_fixture = fixture(PixelFormat::RGBA8, (150, 110), (150, 110), (80, 40));
    let u16_fixture = fixture(PixelFormat::RGBA16, (90, 70), (90, 70), (40, 25));
    assert_ne!(pyramid_levels(&u8_fixture), pyramid_levels(&u16_fixture), "the fixtures must exercise different pyramid levels");
    let pool = single_worker().expect("a one-worker Rayon pool");
    for f in [&u8_fixture, &u16_fixture] {
        // The closure below creates the warped surfaces itself, so the estimate is asked for the
        // warp-surfaces-plus-blend/output phase only: no separate source surfaces exist.
        let placed = [(f.first.width() as usize, f.first.height() as usize), (f.second.width() as usize, f.second.height() as usize)];
        let (estimate, _) = estimate_photomerge_memory(
            &[],
            &placed,
            f.canvas.width() as usize,
            f.canvas.height() as usize,
            f.fmt.mode.color_channels(),
            f.fmt.bytes_per_pixel(),
            true,
            false,
        );
        let (count, peak, ok) = measure_blend(f, &pool);
        assert!(ok, "{:?}: the fixture must blend", f.fmt);
        // Reject a measurement that counted nothing: it would make the comparison meaningless.
        assert!(count > 0 && peak > 0, "{:?}: empty measurement ({count} allocations, {peak} B)", f.fmt);
        assert!(peak <= estimate as u64, "{:?}: {peak} B of allocations exceed the {estimate} B estimate", f.fmt);
    }
}

/// A synthetic ROI covering `w × h` at `(x0, y0)`, allocated before the measurement.
fn roi(x0: i32, y0: i32, w: usize, h: usize, ch: usize, base: f32) -> RoiImage {
    let mut px = Vec::with_capacity(w * h * ch);
    let mut alpha = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let t = (x as f32 / w.max(1) as f32 + y as f32 / h.max(1) as f32) * 0.2;
            for c in 0..ch {
                px.push(base + t + 0.1 * c as f32);
            }
            alpha.push(1.0);
        }
    }
    RoiImage { x0, y0, w, h, ch, px, alpha }
}

fn blend_inputs() -> (Vec<RoiImage>, Vec<Vec<f32>>) {
    let imgs = vec![roi(0, 0, 40, 30, 3, 0.2), roi(24, 18, 40, 30, 3, 0.4)];
    let weights = vec![vec![1.0f32; 40 * 30], vec![1.0f32; 40 * 30]];
    (imgs, weights)
}

#[test]
fn multiband_refuses_a_band_before_allocating() {
    let (w, h, levels) = (64usize, 48usize, 3usize);
    let (imgs, weights) = blend_inputs();
    let pool = single_worker().expect("a one-worker Rayon pool");
    // Warm the worker's allocator counter so a first-use initialisation can't be mistaken for
    // the call under test allocating.
    pool.install(|| {
        let _ = measure(|| {});
    });
    for limit in [0usize, 1] {
        let mut outcome = None;
        let info = pool.install(|| {
            measure(|| {
                outcome = Some(panorama::try_multiband_batched(w, h, &imgs, &weights, levels, usize::MAX, Some(limit)));
            })
        });
        assert!(matches!(outcome, Some(Err(photocraft_raster::AllocationError::NotEnoughMemory { .. }))), "a {limit} B band limit must be refused");
        assert_eq!(info.count_total, 0, "a {limit} B band limit must be refused before any allocation");
    }
}

#[test]
fn multiband_admitted_bands_allocate_and_return_output() {
    let (w, h, ch, levels) = (64usize, 48usize, 3usize, 3usize);
    let (imgs, weights) = blend_inputs();
    let pool = single_worker().expect("a one-worker Rayon pool");
    let mut outcome = None;
    let info = pool.install(|| {
        measure(|| {
            outcome = Some(panorama::try_multiband_batched(w, h, &imgs, &weights, levels, usize::MAX, None));
        })
    });
    let (px, cov) = outcome.expect("the admitted call must return a result").expect("an unbounded band must be admitted");
    assert_eq!(px.len(), w * h * ch);
    assert_eq!(cov.len(), w * h);
    assert!(info.count_total > 0 && info.bytes_max > 0, "an admitted call must have allocated");
}
