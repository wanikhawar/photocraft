
/// Where the first draw's time goes (#10249): proxy build, proxy composite, full composite.
#[test]
#[ignore]
fn spot_bench_breakdown() {
    for mode in BENCH_MODES {
        for depth in BENCH_DEPTHS {
            let d = bench_doc(mode, depth, 2);
            let r = d.bounds();
            let proxy = median_ms(5, || {
                std::hint::black_box(photocraft_compose::proxy::proxy_document(&d, 2));
            });
            let mut bare = d.clone();
            bare.channels.clear();
            let proxy_bare = median_ms(5, || {
                std::hint::black_box(photocraft_compose::proxy::proxy_document(&bare, 2));
            });
            let p = photocraft_compose::proxy::proxy_document(&d, 2);
            let pr = p.bounds();
            let render_proxy = median_ms(5, || {
                let _ = photocraft_compose::render_bands(&p, pr, 0, |b| -> Result<(), ()> {
                    std::hint::black_box(b);
                    Ok(())
                });
            });
            let full = median_ms(5, || {
                std::hint::black_box(composite_sampled(&d, r, 2));
            });
            let total = median_ms(5, || {
                std::hint::black_box(composite(&d, r, 2));
            });
            let mut v = ChannelView::default();
            v.set_alpha_visible(0, true);
            v.set_alpha_visible(1, true);
            let cold = median_ms(5, || {
                std::hint::black_box(render(&d, &v, r, 2, Look::default(), None));
            });
            for (name, ms) in [("proxy_document", proxy), ("proxy_no_channels", proxy_bare), ("render_proxy", render_proxy), ("composite_full", full), ("composite", total), ("canvas_cold", cold)] {
                println!("BENCH {name} {mode:?} {depth:?} spots=2 {ms:.1}");
            }
        }
    }
}
