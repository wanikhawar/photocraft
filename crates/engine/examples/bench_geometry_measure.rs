use photocraft_engine::Session;
use serde_json::json;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 6000, "height": 4000, "depth": 8, "background": "white"}))?;
    s.execute("image.analysis.selectDataPoints", json!({"selection": ["area", "perimeter", "width", "height"]}))?;
    let start = Instant::now();
    let result = s.execute("image.analysis.recordMeasurements", json!({"source": "selection"}))?;
    println!("measurement_seconds={:.6} result={}", start.elapsed().as_secs_f64(), result);
    Ok(())
}
