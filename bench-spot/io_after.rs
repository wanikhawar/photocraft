
#[test]
#[ignore]
fn spot_bench_io_after() {
    for mode in BENCH_MODES {
        for depth in BENCH_DEPTHS {
            for spots in 0..=2 {
                let d = bench_doc(mode, depth, spots);
                let t = median_ms(7, || {
                    std::hint::black_box(photocraft_compose::multichannel::thumbnail_with_inks(&d, 160));
                });
                println!("BENCH thumbnail_with_inks {mode:?} {depth:?} spots={spots} {t:.1}");
            }
        }
    }
}
