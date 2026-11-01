
#[cfg(test)]
#[test]
#[ignore]
fn issue_8822_capture() {
    use photocraft_color::{ColorMode, SampleType};
    use photocraft_doc::Size;
    let canvas = Rect::new(0, 0, 540, 360);
    let mut doc = Document::with_background("Synthetic selection fixture", Size::new(540, 360), ColorMode::Rgb, SampleType::U8, Color::rgb(0.2, 0.4, 0.7));
    let active = doc.layers.first().map(|l| l.id);
    let mask = photocraft_algo::selection::ellipse_surface(Rect::new(90, 45, 450, 315), canvas, true);
    doc.selection = Some(mask);
    for mode in ["overlay", "blackWhite", "onLayers"] {
        for show_edge in [false, true] {
            let p = json!({"radius":4,"feather":2,"refineStrokes":[{"points":[[440,100],[460,190],[435,270]],"size":25,"mode":"add"},{"points":[[435,180],[465,205]],"size":14,"mode":"subtract"}],"__view":{"mode":mode,"opacity":0.6,"showEdge":show_edge}});
            let preview = preview_proxy(doc.clone(), active, &p, 1, None).unwrap();
            let pixels = photocraft_compose::flatten_to_surface(&preview, PixelFormat::RGBA8, None);
            let bytes = pixels.to_interleaved(canvas);
            let image = photocraft_codecs::Image::from_u8(540,360,photocraft_codecs::ChannelLayout::Rgba,bytes).unwrap();
            let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap();
            let output = std::path::PathBuf::from(std::env::var("EVIDENCE_DIR").unwrap());
            std::fs::create_dir_all(&output).unwrap();
            std::fs::write(output.join(format!("{mode}-edge-{show_edge}.png")),png).unwrap();
        }
    }
}
