//! Scratch measurement for #10463 (not for merging): Photomerge of N synthetic 24 MP photos
//! (6000×4000 RGB8 PNGs, 30% overlap), reporting peak RSS (Linux).
//!
//! `measure_10463 gen DIR N` writes the photos; `measure_10463 merge DIR N LAYOUT [BLEND]` merges the first N.

use std::time::Instant;

use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image, SampleType};
use photocraft_engine::Session;
use serde_json::json;

type R<T> = Result<T, Box<dyn std::error::Error>>;

const W: u32 = 6000;
const H: u32 = 4000;
/// Horizontal step between photos: 30% overlap.
const STEP: u32 = 4200;

fn status_bytes(key: &str) -> R<u64> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let line = status.lines().find(|l| l.starts_with(key)).ok_or("no such status line")?;
    let kib: u64 = line.split_whitespace().nth(1).ok_or("no value")?.parse()?;
    Ok(kib * 1024)
}

fn mib(b: u64) -> u64 {
    b >> 20
}

fn hash(x: u32, y: u32) -> u32 {
    let mut h = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^ (h >> 12)
}

/// The scene at global pixel (x, y): a gradient with a disc of random colour and size in most
/// 32 px cells (texture for feature matching).
fn scene(x: u32, y: u32) -> [u8; 3] {
    let (cx, cy) = (x / 32, y / 32);
    let h = hash(cx, cy);
    if h % 4 != 0 {
        let (ox, oy) = (cx * 32 + 10 + (h >> 8) % 12, cy * 32 + 10 + (h >> 12) % 12);
        let r = 4 + (h >> 16) % 6;
        let (dx, dy) = (x.abs_diff(ox), y.abs_diff(oy));
        if dx * dx + dy * dy < r * r {
            return [(h >> 4) as u8, (h >> 20) as u8, (h >> 24) as u8];
        }
    }
    [(60 + x / 400 % 120) as u8, 90, (140 - y / 300 % 80) as u8]
}

fn path(dir: &str, i: u32) -> String {
    format!("{dir}/photo-{i:02}.png")
}

fn generate(dir: &str, n: u32) -> R<()> {
    std::fs::create_dir_all(dir)?;
    for i in 0..n {
        let (x0, y0) = (i * STEP, (i * 37) % 120);
        let mut px = Vec::with_capacity(W as usize * H as usize * 3);
        for y in 0..H {
            for x in 0..W {
                px.extend_from_slice(&scene(x0 + x, y0 + y));
            }
        }
        let img = Image::from_raw(W, H, ChannelLayout::Rgb, SampleType::U8, px).map_err(|e| format!("{e:?}"))?;
        let bytes = photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).map_err(|e| format!("{e:?}"))?;
        println!("wrote {} ({} MiB)", path(dir, i), mib(bytes.len() as u64));
        std::fs::write(path(dir, i), bytes)?;
    }
    Ok(())
}

fn merge(dir: &str, n: u32, layout: &str, blend: bool) -> R<()> {
    let paths: Vec<String> = (0..n).map(|i| path(dir, i)).collect();
    let mut s = Session::new();
    let before = status_bytes("VmRSS:")?;
    let t = Instant::now();
    let out = s.execute("file.automate.photomerge", json!({"paths": paths, "layout": layout, "blend": blend}));
    let secs = t.elapsed().as_secs_f64();
    let peak = status_bytes("VmHWM:")?;
    let result = match &out {
        Ok(v) => format!("{}×{} px, {} layers", v["width"], v["height"], v["layers"]),
        Err(e) => format!("error: {e}"),
    };
    println!("RESULT photos={n} layout={layout} blend={blend}: {secs:.1} s, peak {} MiB (+{} MiB), {result}", mib(peak), mib(peak.saturating_sub(before)));
    Ok(())
}

fn main() -> R<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize| args.get(i).map(String::as_str).ok_or("usage: measure_10463 gen DIR N | merge DIR N LAYOUT [BLEND]");
    match arg(1)? {
        "gen" => generate(arg(2)?, arg(3)?.parse()?),
        "merge" => merge(arg(2)?, arg(3)?.parse()?, arg(4)?, args.get(5).is_none_or(|b| b != "false")),
        _ => Err("usage: measure_10463 gen DIR N | merge DIR N LAYOUT [BLEND]".into()),
    }
}
