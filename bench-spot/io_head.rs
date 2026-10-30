//! Release timings for the spot-ink thumbnail path (#10249): `cargo test --release -p
//! photocraft-io --test it spot_bench -- --ignored --nocapture --test-threads=1`.

#[test]
#[ignore]
fn spot_bench_io() {
    use photocraft_io::{ExportOptions, export};
    for mode in BENCH_MODES {
        for depth in BENCH_DEPTHS {
            for spots in 0..=2 {
                let d = bench_doc(mode, depth, spots);
                let thumb = median_ms(7, || {
                    std::hint::black_box(photocraft_compose::thumbnail_buffer(&d, 160));
                });
                let with = median_ms(5, || {
                    std::hint::black_box(export(&d, "x.psd", &ExportOptions::default()).unwrap());
                });
                let without = median_ms(5, || {
                    std::hint::black_box(export(&d, "x.psd", &ExportOptions { image_previews: false, ..Default::default() }).unwrap());
                });
                println!("BENCH thumbnail_buffer {mode:?} {depth:?} spots={spots} {thumb:.1}");
                println!("BENCH export_psd {mode:?} {depth:?} spots={spots} {with:.1}");
                println!("BENCH export_psd_no_preview {mode:?} {depth:?} spots={spots} {without:.1}");
            }
        }
    }
}
