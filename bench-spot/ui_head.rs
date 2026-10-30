
/// Release timings of the canvas channel overlay with every spot eye on (#10249), at the
/// canvas's fit factor for a 6000 px document (4096 px texture: factor 2). Cold: the composite
/// is rendered too; warm: the cached sampled composite is reused.
#[test]
#[ignore]
fn spot_bench_canvas() {
    for mode in BENCH_MODES {
        for depth in BENCH_DEPTHS {
            for spots in 0..=2 {
                let d = bench_doc(mode, depth, spots);
                let mut v = ChannelView::default();
                for i in 0..spots {
                    v.set_alpha_visible(i, true);
                }
                let (r, factor) = (d.bounds(), 2);
                let cold = median_ms(5, || {
                    std::hint::black_box(render(&d, &v, r, factor, Look::default(), None));
                });
                let cached = composite(&d, r, factor);
                let warm = median_ms(5, || {
                    let mut unused = None;
                    std::hint::black_box(render_with_source(&d, &v, r, factor, Look::default(), None, CompositeSource { cached: Some(&cached), fresh: &mut unused }));
                });
                println!("BENCH canvas_cold {mode:?} {depth:?} spots={spots} {cold:.1}");
                println!("BENCH canvas_warm {mode:?} {depth:?} spots={spots} {warm:.1}");
            }
        }
    }
}
