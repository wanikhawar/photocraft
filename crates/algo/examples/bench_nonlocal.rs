//! Timing and output hashes for the Remove Tool's completion (`photocraft_algo::remove::remove_with`,
//! which runs `nonlocal::complete_with`, tiled for sparse holes), over a matrix of content, hole
//! shape, channel count and bit depth. Built to compare two revisions (see
//! `.github/workflows/remove-ab.yml`): the output hash must not change.
//!
//! `cargo run --release -p photocraft-algo --example bench_nonlocal -- [options]`
//!
//! * `--size WxH`: image size (default `1536x1024`; `6000x4000` is 24 MP, `7360x4912` 36 MP).
//! * `--case a,b`: only cases whose name contains one of the comma-separated substrings (searched
//!   in the whole matrix). Names are
//!   `content/hole/chN/depthD`: content `flat|flatnoise|textured|gradient|edges`, hole
//!   `small|large|corner|line|two`, `ch` 1/3/4/5, depth `8|16|32f` (inputs quantised to that many
//!   levels; the arithmetic is always f32).
//! * `--full`: the whole matrix (300 cases). The default is a core subset of 33: every content and
//!   hole at `ch3/depth8`, plus the channel counts and depths on the textured content.
//! * `--reps N`: timed runs per case (default 3); the output of every run must hash the same.
//! * `--hash`: print (and put in the JSON) an FNV-1a hash of the output's `f32::to_bits()`.
//! * `--json FILE`: write the report in the `photocraft_testkit::perf` format.
//!
//! With `--features phase-timing` each case also prints the time per phase (ms per run) and the
//! number of similarity-weight `exp` evaluations. All data is deterministic (no RNG, no libm).
use std::time::Instant;

use photocraft_algo::remove::remove_with;
use photocraft_raster::Interrupt;
use photocraft_testkit::perf::{RssSampler, report, row, write_report};
use serde_json::{Value, json};

const CONTENTS: [&str; 5] = ["flat", "flatnoise", "textured", "gradient", "edges"];
const HOLES: [&str; 5] = ["small", "large", "corner", "line", "two"];
const CHANNELS: [usize; 4] = [1, 3, 4, 5];
const DEPTHS: [&str; 3] = ["8", "16", "32f"];

struct Opts {
    w: usize,
    h: usize,
    case: Vec<String>,
    full: bool,
    reps: usize,
    hash: bool,
    json: Option<String>,
}

fn parse() -> Result<Opts, String> {
    let mut o = Opts { w: 1536, h: 1024, case: Vec::new(), full: false, reps: 3, hash: false, json: None };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v.split_once(['x', 'X']).ok_or("--size is WxH")?;
                o.w = w.parse().map_err(|_| "bad width")?;
                o.h = h.parse().map_err(|_| "bad height")?;
                if !(64..=20_000).contains(&o.w) || !(64..=20_000).contains(&o.h) {
                    return Err("--size sides must be 64..=20000".into());
                }
            }
            "--case" => o.case = value("--case")?.split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect(),
            "--reps" => o.reps = value("--reps")?.parse::<usize>().map_err(|_| "bad --reps")?.clamp(1, 100),
            "--json" => o.json = Some(value("--json")?),
            "--full" => o.full = true,
            "--hash" => o.hash = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(o)
}

/// A noise value in `[0, 1)` from the cell and channel (integer arithmetic only).
fn noise(x: usize, y: usize, c: usize) -> f32 {
    let mut v = (x as u32).wrapping_mul(73_856_093) ^ (y as u32).wrapping_mul(19_349_663) ^ (c as u32).wrapping_mul(83_492_791);
    v ^= v >> 15;
    v = v.wrapping_mul(0x2c1b_3c6d);
    v ^= v >> 12;
    v = v.wrapping_mul(0x297a_2d39);
    v ^= v >> 15;
    (v >> 8) as f32 / (1u32 << 24) as f32
}

/// A smooth periodic wave in `[-1, 1]`, period 1 (the parabolic sine approximation `4x(1 - |x|)`:
/// no libm, so no platform drift).
fn wave(t: f32) -> f32 {
    let x = (t - t.floor()) * 2.0 - 1.0;
    4.0 * x * (1.0 - x.abs())
}

fn sample(content: &str, x: usize, y: usize, c: usize, w: usize, h: usize) -> f32 {
    let (fx, fy) = (x as f32, y as f32);
    let v = match content {
        "flat" => 0.5 + 0.03 * c as f32,
        "flatnoise" => 0.5 + 0.03 * c as f32 + 0.04 * (noise(x, y, c) - 0.5),
        "textured" => 0.5 + 0.22 * wave(fx * 0.031 + c as f32 * 0.2) * wave(fy * 0.027) + 0.18 * (noise(x, y, c) - 0.5),
        "gradient" => {
            let g = fx / w as f32 * 0.6 + fy / h as f32 * 0.3;
            if c % 2 == 0 { g } else { 0.9 - g * 0.8 }
        }
        _ => {
            let on = (x / 37 + y / 53) % 2 == 0;
            (if on { 0.12 } else { 0.86 }) + 0.01 * c as f32 + 0.02 * (noise(x, y, c) - 0.5)
        }
    };
    v.clamp(0.0, 1.0)
}

fn quantise(v: f32, depth: &str) -> f32 {
    let levels = match depth {
        "8" => 255.0,
        "16" => 65_535.0,
        _ => return v,
    };
    (v * levels).round() / levels
}

fn image(content: &str, ch: usize, depth: &str, w: usize, h: usize) -> Vec<f32> {
    let mut img = Vec::with_capacity(w * h * ch);
    for y in 0..h {
        for x in 0..w {
            for c in 0..ch {
                img.push(quantise(sample(content, x, y, c, w, h), depth));
            }
        }
    }
    img
}

fn disc(w: usize, h: usize, cx: f32, cy: f32, r: f32) -> impl Iterator<Item = usize> {
    let (x0, x1) = ((cx - r).floor().max(0.0) as usize, ((cx + r).ceil().max(0.0) as usize + 1).min(w));
    let (y0, y1) = ((cy - r).floor().max(0.0) as usize, ((cy + r).ceil().max(0.0) as usize + 1).min(h));
    (y0..y1).flat_map(move |y| (x0..x1).map(move |x| (x, y))).filter(move |&(x, y)| (x as f32 - cx).hypot(y as f32 - cy) <= r).map(move |(x, y)| y * w + x)
}

fn hole(kind: &str, w: usize, h: usize) -> Vec<bool> {
    let mut m = vec![false; w * h];
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    match kind {
        "small" => disc(w, h, cx, cy, 30.0).for_each(|i| m[i] = true),
        "large" => disc(w, h, cx, cy, (500.0f32).min(w.min(h) as f32 / 3.0)).for_each(|i| m[i] = true),
        "corner" => disc(w, h, 30.0, 20.0, 100.0).for_each(|i| m[i] = true),
        "two" => {
            disc(w, h, w as f32 * 0.25, cy, 40.0).for_each(|i| m[i] = true);
            disc(w, h, w as f32 * 0.75, cy, 40.0).for_each(|i| m[i] = true);
        }
        _ => {
            // A diagonal line 4 px wide from a quarter to three quarters of the image (3606 px on
            // a 6000x4000 one): sparse, so it is completed tile by tile.
            let (ax, ay, bx, by) = (w as f32 * 0.25, h as f32 * 0.25, w as f32 * 0.75, h as f32 * 0.75);
            let steps = (bx - ax).hypot(by - ay).ceil() as usize;
            for s in 0..=steps {
                let t = s as f32 / steps.max(1) as f32;
                disc(w, h, ax + (bx - ax) * t, ay + (by - ay) * t, 2.0).for_each(|i| m[i] = true);
            }
        }
    }
    m
}

fn fnv(out: &[f32]) -> u64 {
    out.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, v| v.to_bits().to_le_bytes().iter().fold(h, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)))
}

/// Every case of the matrix in generation order (content, channels, depth, hole), so consecutive
/// cases share their image.
fn cases(full: bool) -> Vec<(&'static str, usize, &'static str, &'static str)> {
    let mut v = Vec::new();
    for content in CONTENTS {
        for ch in CHANNELS {
            for depth in DEPTHS {
                for hole in HOLES {
                    let core = (ch == 3 && depth == "8")
                        || (content == "textured" && depth == "8" && ch != 3 && (hole == "small" || hole == "large"))
                        || (content == "textured" && ch == 3 && depth != "8" && hole == "large");
                    if full || core {
                        v.push((content, ch, depth, hole));
                    }
                }
            }
        }
    }
    v
}

fn main() {
    if let Err(e) = run() {
        eprintln!("bench_nonlocal: {e}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let o = parse()?;
    let (w, h) = (o.w, o.h);
    let sampler = RssSampler::start(std::time::Duration::from_millis(3));
    let threads = rayon::current_num_threads();
    println!("{w}x{h} ({:.1} MP), {} reps, {threads} rayon threads", (w * h) as f64 / 1e6, o.reps);
    let mut rows: Vec<Value> = Vec::new();
    let mut cached: Option<((&str, usize, &str), Vec<f32>)> = None;
    for (content, ch, depth, kind) in cases(o.full || !o.case.is_empty()) {
        let name = format!("{content}/{kind}/ch{ch}/depth{depth}");
        if !o.case.is_empty() && !o.case.iter().any(|c| name.contains(c.as_str())) {
            continue;
        }
        let key = (content, ch, depth);
        if cached.as_ref().is_none_or(|(k, _)| *k != key) {
            cached = Some((key, image(content, ch, depth, w, h)));
        }
        let Some((_, base)) = cached.as_ref() else { continue };
        let mask = hole(kind, w, h);
        let mut img = base.clone();
        for (i, _) in mask.iter().enumerate().filter(|(_, m)| **m) {
            img[i * ch..(i + 1) * ch].fill(1.0);
        }
        let count = mask.iter().filter(|m| **m).count();
        sampler.reset();
        #[cfg(feature = "phase-timing")]
        photocraft_algo::nonlocal::phase_timing::reset();
        let (mut times, mut first_hash) = (Vec::with_capacity(o.reps), None);
        for _ in 0..o.reps {
            let t = Instant::now();
            let out = remove_with(w, h, ch, &img, &mask, 1, &Interrupt::NONE).map_err(|_| "cancelled".to_string())?;
            times.push(t.elapsed().as_secs_f64() * 1e3);
            if o.hash {
                let hv = fnv(&out);
                if *first_hash.get_or_insert(hv) != hv {
                    return Err(format!("{name}: two runs of one revision hashed differently"));
                }
            }
        }
        let rss = sampler.peak();
        let mut r = row(&name, &times, rss, None);
        let s = photocraft_testkit::perf::Summary::of(&times);
        let hash_s = first_hash.map(|v| format!("{v:016x}"));
        if let Some(hv) = &hash_s {
            r["hash"] = json!(hv);
        }
        r["hole_px"] = json!(count);
        println!(
            "{name:<36} hole {count:>9} px  p50 {:>9.1} ms  p95 {:>9.1} ms  rss {:>7.0} MB  {}",
            s.map_or(f64::NAN, |s| s.p50),
            s.map_or(f64::NAN, |s| s.p95),
            rss.unwrap_or(0) as f64 / 1e6,
            hash_s.as_deref().unwrap_or("")
        );
        #[cfg(feature = "phase-timing")]
        {
            let snap = photocraft_algo::nonlocal::phase_timing::snapshot();
            let per_run = |ns: u64| ns as f64 / 1e6 / o.reps as f64;
            let mut phases = serde_json::Map::new();
            let mut line = String::from("    ms/run:");
            for (n, ns) in photocraft_algo::nonlocal::phase_timing::Snapshot::NAMES.iter().zip(snap.nanos) {
                phases.insert((*n).to_string(), json!(per_run(ns)));
                line.push_str(&format!(" {n} {:.1}", per_run(ns)));
            }
            phases.insert("exp_vote".into(), json!(snap.exp_vote / o.reps as u64));
            phases.insert("exp_onion".into(), json!(snap.exp_onion / o.reps as u64));
            println!("{line}  | exp/run: vote {} onion {}", snap.exp_vote / o.reps as u64, snap.exp_onion / o.reps as u64);
            r["phases"] = Value::Object(phases);
        }
        rows.push(r);
    }
    if let Some(path) = &o.json {
        let ctx = json!({
            "size": format!("{w}x{h}"),
            "reps": o.reps,
            "threads": threads,
            "phase_timing": cfg!(feature = "phase-timing"),
        });
        write_report(path, &report("bench_nonlocal", ctx, rows, Some(&sampler)))?;
        println!("wrote {path}");
    }
    Ok(())
}
