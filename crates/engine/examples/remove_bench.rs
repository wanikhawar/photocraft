//! Timing: the Remove Tool (`paint.remove`) on a 6000×4000 (24 MP) document, through the full
//! command path (`Session::execute`: window read, hole, fill, write, undo snapshot).
//! `cargo run --release -p photocraft-engine --example remove_bench [depth] [options]`
//!
//! Rings around objects of 100, 300 and 1000 px (one completion each) and a 3000 px thin line
//! (completed tile by tile), each undone before the next.
//!
//! * `depth`: 8, 16 or 32 bits per channel (positional, default 8).
//! * `--size WxH`: document size (default `6000x4000`; `7360x4912` is 36 MP). The rings stay centred
//!   and the line runs from a quarter to three quarters of the document.
//! * `--mode rgb|gray|cmyk|lab`: colour model (the document is made in RGB, then converted).
//! * `--reps N`: timed runs per case (default 1); with more, p50 and p95 are printed.
//! * `--hash`: an FNV-1a hash of the active layer's pixels after each case's first run, so two
//!   revisions can be compared (needs a few hundred MB more memory than the run itself).
//! * `--json FILE`: the report in the `photocraft_testkit::perf` format.
use std::time::Instant;

use photocraft_engine::Session;
use photocraft_geom::Rect;
use photocraft_testkit::perf::{RssSampler, Summary, report, row, write_report};
use serde_json::{Value, json};

fn ring(cx: f64, cy: f64, r: f64) -> Vec<[f64; 2]> {
    (0..=96).map(|i| f64::from(i) / 96.0 * std::f64::consts::TAU).map(|t| [cx + r * t.cos(), cy + r * t.sin()]).collect()
}

struct Opts {
    depth: u64,
    w: u32,
    h: u32,
    mode: String,
    reps: usize,
    hash: bool,
    json: Option<String>,
}

fn parse() -> Result<Opts, String> {
    let mut o = Opts { depth: 8, w: 6000, h: 4000, mode: "rgb".into(), reps: 1, hash: false, json: None };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v.split_once(['x', 'X']).ok_or("--size is WxH")?;
                o.w = w.parse().map_err(|_| "bad width")?;
                o.h = h.parse().map_err(|_| "bad height")?;
                if !(256..=20_000).contains(&o.w) || !(256..=20_000).contains(&o.h) {
                    return Err("--size sides must be 256..=20000".into());
                }
            }
            "--mode" => {
                o.mode = value("--mode")?;
                if !["rgb", "gray", "cmyk", "lab"].contains(&o.mode.as_str()) {
                    return Err("--mode is rgb, gray, cmyk or lab".into());
                }
            }
            "--reps" => o.reps = value("--reps")?.parse::<usize>().map_err(|_| "bad --reps")?.clamp(1, 100),
            "--json" => o.json = Some(value("--json")?),
            "--hash" => o.hash = true,
            v if !v.starts_with("--") => o.depth = v.parse().map_err(|_| format!("bad depth {v}"))?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(o)
}

/// FNV-1a over the active layer's pixel bits, read in strips of rows.
fn layer_hash(s: &Session, w: u32, h: u32) -> Option<u64> {
    let st = s.active()?;
    let surface = st.doc.layer(st.active_layer?)?.surface()?;
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for y0 in (0..h as i32).step_by(128) {
        let strip = surface.read_region(Rect::new(0, y0, w as i32, (y0 + 128).min(h as i32)));
        hash = strip.iter().fold(hash, |h, v| v.to_bits().to_le_bytes().iter().fold(h, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)));
    }
    Some(hash)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("remove_bench: {e}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let o = parse()?;
    let (w, h) = (f64::from(o.w), f64::from(o.h));
    let sampler = RssSampler::start(std::time::Duration::from_millis(3));
    let mut s = Session::new();
    let err = |e: photocraft_engine::EngineError| e.to_string();
    s.execute("file.new", json!({"width": o.w, "height": o.h, "depth": o.depth})).map_err(err)?;
    s.execute("filter.render.clouds", json!({"seed": 3})).map_err(err)?;
    s.execute("filter.noise.addNoise", json!({"amount": 8, "seed": 1})).map_err(err)?;
    let convert = match o.mode.as_str() {
        "gray" => Some("image.mode.grayscale"),
        "cmyk" => Some("image.mode.cmyk"),
        "lab" => Some("image.mode.lab"),
        _ => None,
    };
    if let Some(id) = convert {
        s.execute(id, json!({})).map_err(err)?;
    }
    println!("document {}x{}, {}-bit {}", o.w, o.h, o.depth, o.mode.to_uppercase());
    let (cx, cy, fit) = (w / 2.0, h / 2.0, w.min(h) * 0.4);
    let cases: Vec<(String, Value)> = vec![
        ("ring 100 px".into(), json!({"points": ring(cx, cy, 60.0f64.min(fit)), "size": 16})),
        ("ring 300 px".into(), json!({"points": ring(cx, cy, 160.0f64.min(fit)), "size": 24})),
        ("ring 1000 px".into(), json!({"points": ring(cx, cy, 510.0f64.min(fit)), "size": 32})),
        ("line 3000 px, 4 px wide".into(), json!({"points": [[w * 0.25, h * 0.25], [w * 0.75, h * 0.75]], "size": 2})),
    ];
    let mut rows = Vec::new();
    for (name, p) in cases {
        sampler.reset();
        let (mut times, mut hash) = (Vec::with_capacity(o.reps), None);
        for rep in 0..o.reps {
            let t = Instant::now();
            s.execute("paint.remove", p.clone()).map_err(err)?;
            times.push(t.elapsed().as_secs_f64() * 1e3);
            if o.hash && rep == 0 {
                hash = layer_hash(&s, o.w, o.h);
            }
            s.execute("edit.undo", json!({})).map_err(err)?;
        }
        let mut r = row(&name, &times, sampler.peak(), None);
        let hash_s = hash.map(|v| format!("{v:016x}"));
        if let Some(v) = &hash_s {
            r["hash"] = json!(v);
        }
        match Summary::of(&times) {
            Some(sm) if o.reps > 1 => println!("{name:<26} p50 {:>8.1} ms  p95 {:>8.1} ms  {}", sm.p50, sm.p95, hash_s.as_deref().unwrap_or("")),
            _ => println!("{name:<26} {:>8.1?} ms  {}", times.first().copied().unwrap_or(f64::NAN), hash_s.as_deref().unwrap_or("")),
        }
        rows.push(r);
    }
    if let Some(path) = &o.json {
        let ctx = json!({"size": format!("{}x{}", o.w, o.h), "depth": o.depth, "mode": o.mode, "reps": o.reps});
        write_report(path, &report("remove_bench", ctx, rows, Some(&sampler)))?;
        println!("wrote {path}");
    }
    Ok(())
}
