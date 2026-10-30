//! Scratch probe for #10249 (not for merging): prints how spot channels are handled today.
//! Run: cargo test -p photocraft-ui-egui --test spot_ink_probe -- --nocapture --test-threads=1

use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{AlphaChannel, Document, Size};
use photocraft_engine::channel_cmds::ChannelView;
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use photocraft_ui_egui::channel_view::{Look, render};

fn spot(name: &str, ink: Color, solidity: f32, vals: &[f32]) -> AlphaChannel {
    let mut s = Surface::new(PixelFormat::new(ColorMode::Grayscale, SampleType::U8, false));
    s.write_region(Rect::from_xywh(0, 0, vals.len() as u32, 1), vals);
    AlphaChannel { spot: Some((ink, solidity)), ..AlphaChannel::new(name, s) }
}

fn view_all(n: usize) -> ChannelView {
    ChannelView { alpha_visible: vec![true; n], ..Default::default() }
}

fn px(c: egui::Color32) -> String {
    let [r, g, b, a] = c.to_array();
    format!("({r:3},{g:3},{b:3},{a:3})")
}

fn rgb8(p: [f32; 4]) -> String {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("({:3},{:3},{:3},{:3})", q(p[0]), q(p[1]), q(p[2]), q(p[3]))
}

/// Coverage pattern: A alone full, both full, A half, B alone full, none.
const A: [f32; 5] = [1.0, 1.0, 0.5, 0.0, 0.0];
const B: [f32; 5] = [0.0, 1.0, 0.0, 1.0, 0.0];

fn synthetic(mode: ColorMode, solidity: f32, swap: bool) -> Document {
    let mut doc = Document::new("probe", Size::new(5, 1), mode, SampleType::U8);
    let a = spot("Cyan ink", Color::rgb(0.0, 1.0, 1.0), solidity, &A);
    let b = spot("Magenta ink", Color::rgb(1.0, 0.0, 1.0), solidity, &B);
    if swap {
        doc.channels.extend([b, a]);
    } else {
        doc.channels.extend([a, b]);
    }
    doc
}

#[test]
fn p1_canvas_preview_rgb_cmyk() {
    println!("\n=== P1 canvas spot preview (channel_view::render) over a transparent doc (matted white)");
    println!("px: [A=1], [A=1,B=1], [A=0.5], [B=1], [none]; A = cyan ink, B = magenta ink");
    for mode in [ColorMode::Rgb, ColorMode::Cmyk] {
        for solidity in [0.0, 0.5, 1.0] {
            for swap in [false, true] {
                let doc = synthetic(mode, solidity, swap);
                let out = render(&doc, &view_all(2), doc.bounds(), 1, Look::default(), None);
                let s = out.map(|v| v.into_iter().map(px).collect::<Vec<_>>().join(" ")).unwrap_or("None (plain composite)".into());
                println!("{mode:?} solidity {solidity} order {}: {s}", if swap { "B,A" } else { "A,B" });
            }
        }
    }
    // The compositor's overprint model for the same inks (Multichannel backdrop over white).
    for solidity in [0.0, 0.5, 1.0] {
        for swap in [false, true] {
            let doc = synthetic(ColorMode::Multichannel, solidity, swap);
            let r = photocraft_compose::flatten(&doc);
            println!(
                "Multichannel inks() solidity {solidity} order {}: {}",
                if swap { "B,A" } else { "A,B" },
                r.px.iter().map(|p| rgb8(*p)).collect::<Vec<_>>().join(" ")
            );
        }
    }
}

#[test]
fn p2_spots_never_reach_composite_or_thumbnail() {
    println!("\n=== P2 flatten / thumbnail_buffer with spot channels (RGB and CMYK, white background layer absent)");
    for mode in [ColorMode::Rgb, ColorMode::Cmyk] {
        let with = synthetic(mode, 0.0, false);
        let mut without = with.clone();
        without.channels.clear();
        let (fw, fo) = (photocraft_compose::flatten(&with), photocraft_compose::flatten(&without));
        let (tw, to) = (photocraft_compose::thumbnail_buffer(&with, 160), photocraft_compose::thumbnail_buffer(&without, 160));
        println!("{mode:?} flatten equal with/without spots: {}  thumbnail equal: {}", fw.px == fo.px, tw.px == to.px);
        println!("{mode:?} flatten px: {}", fw.px.iter().map(|p| rgb8(*p)).collect::<Vec<_>>().join(" "));
    }
}

#[test]
fn p3_ink_colour_conversion() {
    println!("\n=== P3 spot ink colours as the preview converts them (Color::to_rgb)");
    let cmyk = |c: [f32; 4]| Color { mode: ColorMode::Cmyk, c, alpha: 1.0 };
    for (name, c) in
        [("CMYK 100/0/0/0", cmyk([1.0, 0.0, 0.0, 0.0])), ("CMYK 0/100/0/0", cmyk([0.0, 1.0, 0.0, 0.0])), ("CMYK 0/0/0/100", cmyk([0.0, 0.0, 0.0, 1.0]))]
    {
        let [r, g, b] = c.to_rgb();
        println!("{name}: to_rgb = {}", rgb8([r, g, b, 1.0]));
    }
}

fn corpus(name: &str) -> Option<Vec<u8>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/psd-tools").join(name);
    match std::fs::read(&p) {
        Ok(b) => Some(b),
        Err(e) => {
            println!("SKIP {}: {e}", p.display());
            None
        }
    }
}

fn probe_file(name: &str, probes: &[(i32, i32)]) {
    let Some(bytes) = corpus(name) else { return };
    let mut s = photocraft_engine::Session::new();
    let opened = photocraft_engine::file_cmds::open_bytes_as(&mut s, name, &bytes, None, None).unwrap();
    println!("open result: {opened}");
    if let Ok(r) = photocraft_io::import(name, &bytes) {
        println!("io warnings: {:?}", r.warnings);
    }
    let st = s.active().unwrap();
    let doc = &st.doc;
    println!("mode {:?} depth {:?} size {}x{} layers {}", doc.mode, doc.pixel_format().sample, doc.bounds().width(), doc.bounds().height(), doc.layers.len());
    for (i, ch) in doc.channels.iter().enumerate() {
        println!(
            "channel {i}: {:?} spot {:?} color {:?} opacity {} indicates {:?} -> ink to_rgb {:?}  visible-after-open {}",
            ch.name,
            ch.spot,
            ch.color,
            ch.opacity,
            ch.indicates,
            ch.spot.map(|(c, _)| c.to_rgb().map(|v| (v * 255.0).round())),
            st.channel_view.alpha_shown(i)
        );
    }
    println!("channel_view after open: {:?}  is_plain {}", st.channel_view, st.channel_view.is_plain(doc));
    let mut eyes = st.channel_view.clone();
    for (i, ch) in doc.channels.iter().enumerate() {
        if ch.spot.is_some() {
            eyes.set_alpha_visible(i, true);
        }
    }
    // Probe points: the given ones plus each spot channel's most-inked pixel.
    let mut points: Vec<(String, i32, i32)> = probes.iter().map(|&(x, y)| ("given".to_string(), x, y)).collect();
    let b = doc.bounds();
    for ch in doc.channels.iter().filter(|c| c.spot.is_some()) {
        let mut vals = Vec::new();
        ch.surface.read_region_into(b, &mut vals);
        let n = ch.surface.channels();
        let (k, vmax) = vals.chunks_exact(n).map(|v| v[0]).enumerate().fold((0, f32::MIN), |acc, (k, v)| if v > acc.1 { (k, v) } else { acc });
        let (x, y) = (b.x0 + (k as u32 % b.width()) as i32, b.y0 + (k as u32 / b.width()) as i32);
        points.push((format!("max of {:?} ({vmax})", ch.name), x, y));
    }
    let thumb = photocraft_compose::thumbnail_buffer(doc, 160);
    let (tw, th) = (thumb.rect.width(), thumb.rect.height());
    for (label, x, y) in points {
        let one = Rect::from_xywh(x, y, 1, 1);
        let vals: Vec<String> = doc
            .channels
            .iter()
            .map(|c| {
                let mut v = Vec::new();
                c.surface.read_region_into(one, &mut v);
                format!("{:.3}", v.first().copied().unwrap_or(f32::NAN))
            })
            .collect();
        let flat = photocraft_compose::render(doc, one);
        let as_opened = render(doc, &st.channel_view, one, 1, Look::default(), None);
        let with_eyes = render(doc, &eyes, one, 1, Look::default(), None);
        let (tx, ty) = ((x as u32 * tw / b.width()) as i32, (y as u32 * th / b.height()) as i32);
        println!(
            "@({x},{y}) [{label}] channel values {vals:?}\n    composite {}  overlay as opened {}  overlay with spot eyes on {}  thumbnail {}",
            rgb8(flat.px[0]),
            as_opened.map(|v| px(v[0])).unwrap_or("None (plain)".into()),
            with_eyes.map(|v| px(v[0])).unwrap_or("None (plain)".into()),
            rgb8(thumb.get(thumb.rect.x0 + tx, thumb.rect.y0 + ty))
        );
    }
}

#[test]
fn p4_photoshop_file() {
    println!("\n=== P4 Photoshop 24.7 flat file cmyk-alpha-spot.psd: planes C,M,Y,K,'light teal' spot (solid at 1348,1492), 'Alpha 1' (white at 1441,2571)");
    println!("Photoshop's own thumbnail shows LIGHT TEAL; merged CMYK there is 255,255,255,255 (no process ink).");
    probe_file("cmyk-alpha-spot.psd", &[(1348, 1492), (1441, 2571), (100, 100)]);
}

#[test]
fn p5_layered_spot_file() {
    println!("\n=== P5 psd-tools layered CMYK file cmyk-spot.psd with three extra channels");
    probe_file("cmyk-spot.psd", &[(10, 10)]);
}
