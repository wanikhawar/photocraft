//! Estimate Depth through the shared Craft AI layer (storytold/craft-ai-integrations).
//!
//! The model, its files, where they live and how they are set up belong to the shared layer: the
//! tool is `estimate-depth` (capability `image.depth`). This module is Estimate Depth's side only:
//! it sends a picture through the app's AI layer ([`crate::ai_layer`], which asks whether the tool
//! can run and runs it) and reads the 16-bit gray picture back. Nothing here names a model, an
//! engine or a file, and nothing is downloaded: setup is the shared Set up AI flow.

use std::io::Cursor;
use std::sync::Arc;

use craft_ai_core::{InputRole, Payload};
use serde_json::json;

use super::{Quality, bad};
use crate::ai_layer::{AiState, CANCEL_TICK, DEPTH_LIMIT, call_tool};
use crate::jobs::JobCtx;
use crate::{EngineError, Result};

pub use crate::ai_layer::{DEPTH_TOOL as TOOL, Estimator};

/// Largest picture accepted back, in pixels.
const MAX_RESULT_PIXELS: u64 = 1 << 26;

/// The estimate step of a job: PNG in, gray PNG out.
pub(super) type Backend = Arc<dyn Fn(&JobCtx, Vec<u8>, u32, u32, Quality) -> Result<Vec<u8>> + Send + Sync>;

/// What a job calls: send `png` (`w` x `h`) and get the gray PNG back.
pub(super) fn backend(ai: &AiState) -> std::result::Result<Backend, String> {
    if let Some(e) = ai.estimator.clone() {
        // The stand-in runs on its own thread so a cancel of the job reaches it as it does the
        // real tool (which waits through `wait_cancellable`); a cancelled one is left to finish.
        return Ok(Arc::new(move |ctx, png, w, h, q| {
            let (tx, rx) = std::sync::mpsc::channel();
            let e = e.clone();
            let name = q.name();
            std::thread::Builder::new()
                .name("photocraft-depth-estimator".into())
                .spawn(move || {
                    let _ = tx.send(e(&png, w, h, name));
                })
                .map_err(|e| bad(format!("could not start the estimate: {e}")))?;
            loop {
                match rx.recv_timeout(CANCEL_TICK) {
                    Ok(answer) => return answer.map_err(bad),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) if ctx.cancelled() => return Err(EngineError::Cancelled),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err(bad("Estimate Depth stopped unexpectedly")),
                }
            }
        }));
    }
    let tools = ai.tools()?;
    Ok(Arc::new(move |ctx, png, _, _, q| {
        let request = tools.request(TOOL).ok_or_else(|| bad("Estimate Depth is not available here"))?;
        let source = Payload::inline("image/png", png).map_err(|e| bad(e.message))?;
        let request = request.with_input(InputRole::Source, source).with_param("quality", json!(q.name()));
        let outputs = call_tool(&tools, TOOL, request, &|| ctx.cancelled(), DEPTH_LIMIT)?.ok_or_else(|| {
            bad("The chosen model for Estimate Depth needs your agreement first. Use Estimate Depth from the brush settings in the app to review it.")
        })?;
        outputs
            .into_iter()
            .find_map(|o| match o.payload {
                Payload::Inline { media_type, data } if media_type.as_str() == "image/png" => Some(data),
                _ => None,
            })
            .ok_or_else(|| bad("Estimate Depth returned no picture"))
    }))
}

/// A decoded depth map: row-major values in 0..1 (larger = nearer).
pub(super) struct Map {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
}

/// Decode the tool's gray PNG (16- or 8-bit) into 0..1. The size must be the one that was sent.
pub(super) fn decode_gray(bytes: &[u8], w: u32, h: u32) -> Result<Map> {
    let invalid = || bad("Estimate Depth returned a picture that could not be read");
    let mut reader = png::Decoder::new(Cursor::new(bytes)).read_info().map_err(|_| invalid())?;
    let info = reader.info();
    let (iw, ih, color, depth) = (info.width, info.height, info.color_type, info.bit_depth);
    if (iw, ih) != (w, h) {
        return Err(bad(format!("Estimate Depth returned a {iw} x {ih} picture for a {w} x {h} one")));
    }
    if u64::from(iw) * u64::from(ih) > MAX_RESULT_PIXELS {
        return Err(bad("Estimate Depth returned a picture that is too large"));
    }
    let wide = match (color, depth) {
        (png::ColorType::Grayscale, png::BitDepth::Sixteen) => true,
        (png::ColorType::Grayscale, png::BitDepth::Eight) => false,
        _ => return Err(bad("Estimate Depth returned a picture that is not 8- or 16-bit gray")),
    };
    let size = reader.output_buffer_size().ok_or_else(invalid)?;
    let expected = (iw as usize).saturating_mul(ih as usize).saturating_mul(if wide { 2 } else { 1 });
    if size != expected {
        return Err(invalid());
    }
    let mut buf = vec![0u8; size];
    reader.next_frame(&mut buf).map_err(|_| invalid())?;
    let data = if wide {
        buf.as_chunks::<2>().0.iter().map(|p| f32::from(u16::from_be_bytes(*p)) / 65535.0).collect()
    } else {
        buf.iter().map(|&v| f32::from(v) / 255.0).collect()
    };
    Ok(Map { width: iw, height: ih, data })
}

#[cfg(test)]
pub(crate) fn encode_gray16(data: &[u16], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Sixteen);
    let mut writer = enc.write_header().unwrap();
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_be_bytes()).collect();
    writer.write_image_data(&bytes).unwrap();
    drop(writer);
    out
}
