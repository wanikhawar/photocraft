//! Scratch measurement for #10032 (not for merging): place large PNGs as embedded smart objects on
//! a 900×1050 document, then Image Size to 3600×4200, and report each phase's peak RSS (Linux).
//!
//! `measure_10032 gen DIR` writes the test PNGs; `measure_10032 measure DIR N` places the first N.

use std::time::Instant;

use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image, SampleType};
use photocraft_engine::Session;
use serde_json::json;

type R<T> = Result<T, Box<dyn std::error::Error>>;

/// The first three are the issue's 3-object case; all ten have a 6144–10800 px long side.
const SIZES: [(u32, u32); 10] =
    [(4096, 6144), (4092, 6148), (6144, 4096), (8192, 8192), (10800, 10800), (7200, 10800), (10800, 7200), (6144, 6144), (8192, 5461), (9000, 6000)];

fn status_bytes(key: &str) -> R<u64> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let line = status.lines().find(|l| l.starts_with(key)).ok_or("no such status line")?;
    let kib: u64 = line.split_whitespace().nth(1).ok_or("no value")?.parse()?;
    Ok(kib * 1024)
}

fn reset_peak() -> R<()> {
    std::fs::write("/proc/self/clear_refs", "5")?;
    Ok(())
}

fn mib(b: u64) -> u64 {
    b >> 20
}

fn png(w: u32, h: u32, seed: u32) -> R<Vec<u8>> {
    let mut px = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h {
        for x in 0..w {
            let v = x.wrapping_mul(5).wrapping_add(y).wrapping_add(seed);
            px.extend_from_slice(&[v as u8, ((y.wrapping_mul(7)) ^ x) as u8, (x.wrapping_mul(y) % 251) as u8, 255 - ((x / 64 + y / 64) % 4 * 40) as u8]);
        }
    }
    let img = Image::from_raw(w, h, ChannelLayout::Rgba, SampleType::U8, px).map_err(|e| format!("{e:?}"))?;
    Ok(photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).map_err(|e| format!("{e:?}"))?)
}

fn path(dir: &str, i: usize) -> String {
    let (w, h) = SIZES[i];
    format!("{dir}/{i:02}-{w}x{h}.png")
}

fn generate(dir: &str) -> R<()> {
    std::fs::create_dir_all(dir)?;
    for (i, &(w, h)) in SIZES.iter().enumerate() {
        let bytes = png(w, h, i as u32 * 31)?;
        println!("wrote {} ({} MiB)", path(dir, i), mib(bytes.len() as u64));
        std::fs::write(path(dir, i), bytes)?;
    }
    Ok(())
}

fn measure(dir: &str, n: usize) -> R<()> {
    let n = n.min(SIZES.len());
    let pixels: u64 = SIZES[..n].iter().map(|&(w, h)| u64::from(w) * u64::from(h)).sum();
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 900, "height": 1050}))?;
    reset_peak()?;
    let t = Instant::now();
    for i in 0..n {
        s.execute("file.placeEmbedded", json!({"path": path(dir, i)}))?;
    }
    let place_s = t.elapsed().as_secs_f64();
    let (place_peak, place_rss) = (status_bytes("VmHWM:")?, status_bytes("VmRSS:")?);

    reset_peak()?;
    let before = status_bytes("VmRSS:")?;
    let t = Instant::now();
    let out = s.execute("image.imageSize", json!({"width": 3600, "height": 4200, "resample": "bicubic"}))?;
    let size_s = t.elapsed().as_secs_f64();
    let (size_peak, size_rss) = (status_bytes("VmHWM:")?, status_bytes("VmRSS:")?);
    let history = s.active().map(|d| d.memory_bytes() as u64).unwrap_or(0);
    let stages = photocraft_engine::smart_cmds::stack_cache_bytes() as u64;

    println!("objects={n} source_mpx={}", pixels / 1_000_000);
    println!("place: {place_s:.1} s, peak {} MiB, rss after {} MiB", mib(place_peak), mib(place_rss));
    println!(
        "image size: {size_s:.1} s, rss before {} MiB, peak {} MiB (+{} MiB), rss after {} MiB, result {out}",
        mib(before),
        mib(size_peak),
        mib(size_peak.saturating_sub(before)),
        mib(size_rss)
    );
    println!("doc+history {} MiB, stage cache {} MiB", mib(history), mib(stages));
    Ok(())
}

/// One cold decode of a 4096×4096 RGBA8 source (what a smart object's first re-render does).
fn source() -> R<()> {
    const SIDE: u32 = 4096;
    let bytes = png(SIDE, SIDE, 7)?;
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 8, "height": 8}))?;
    let fmt = s.active().ok_or("no document")?.doc.pixel_format();
    reset_peak()?;
    let before = status_bytes("VmHWM:")?;
    let t = Instant::now();
    let img = photocraft_engine::smart_cmds::source_image("big.png", &bytes, fmt)?;
    let secs = t.elapsed().as_secs_f64();
    let grown = status_bytes("VmHWM:")?.saturating_sub(before);
    let px = u64::from(SIDE) * u64::from(SIDE);
    println!(
        "source decode {}×{}: {secs:.2} s, peak growth {} MiB ({:.1} B/px)",
        img.bounds.width(),
        img.bounds.height(),
        mib(grown),
        grown as f64 / px as f64
    );
    Ok(())
}

fn main() -> R<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("source") => source(),
        Some("gen") => generate(args.get(2).ok_or("gen DIR")?),
        Some("measure") => measure(args.get(2).ok_or("measure DIR N")?, args.get(3).ok_or("measure DIR N")?.parse()?),
        _ => Err("usage: measure_10032 gen DIR | measure DIR N".into()),
    }
}
