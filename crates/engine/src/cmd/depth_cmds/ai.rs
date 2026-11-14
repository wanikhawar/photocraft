//! Estimate Depth through the shared Craft AI layer (storytold/craft-ai-integrations).
//!
//! The model, its files, where they live and how they are set up belong to the shared layer: the
//! tool is `estimate-depth` (capability `image.depth`). This module is the app's side only: it asks
//! the layer whether the tool can run, sends a picture, and reads the 16-bit gray picture back.
//! Nothing here names a model, an engine or a file, and nothing is downloaded: setup is the shared
//! Set up AI flow (the launcher, or the app's own screen over the same pieces).

use std::io::Cursor;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use craft_ai_client::{AiClient, AppTools, RoleStatus};
use craft_ai_core::{AppId, ConsentLedger, InputRole, Payload};
use craft_ai_native::shared::SharedStore;
use serde_json::json;

use super::{Quality, bad};
use crate::jobs::JobCtx;
use crate::{EngineError, Result};

/// The catalogue id of the tool.
pub const TOOL: &str = "estimate-depth";
/// This app's id in the shared layer (consent and per-app choices are scoped by it).
const APP: &str = "photocraft";
/// The longest the tool may take before it is stopped.
const LIMIT: Duration = Duration::from_secs(10 * 60);
/// How often a waiting job looks at its cancel flag.
const CANCEL_TICK: Duration = Duration::from_millis(30);
/// How long a status answer is reused, so enabling checks do not read the shared store each frame.
const STATUS_TTL: Duration = Duration::from_secs(1);
/// Largest picture sent to the tool, in bytes (the layer's inline limit is 32 MiB).
pub(super) const MAX_SEND_BYTES: usize = 32 * 1024 * 1024 - 1;
/// Largest picture accepted back, in pixels.
const MAX_RESULT_PIXELS: u64 = 1 << 26;

/// A test hook that replaces the tool: it gets the PNG, its size and the quality name, and returns
/// a 16-bit (or 8-bit) gray PNG of the same size, white = near.
pub type Estimator = Arc<dyn Fn(&[u8], u32, u32, &str) -> std::result::Result<Vec<u8>, String> + Send + Sync>;

/// Where the shared AI store is and how a client over it is made. The app's own windows (the
/// missing-model popup, the setup screen) build their folder desk and tool desk from the same one.
#[derive(Clone)]
pub struct AiHost {
    shared: SharedStore,
    client: Arc<dyn Fn() -> std::result::Result<AiClient, String> + Send + Sync>,
}

impl AiHost {
    /// The real shared store and key stores. `CRAFT_AI_DIR` moves the store (tests and the CLI use
    /// it so they never read a person's settings).
    pub fn standard() -> std::result::Result<Self, String> {
        let shared = SharedStore::at_default_location().map_err(|e| e.to_string())?;
        let client = Arc::new(|| {
            let app = AppId::new(APP).map_err(|e| e.to_string())?;
            AiClient::standard(app).map_err(|e| e.message)
        });
        Ok(Self { shared, client })
    }

    /// A host over any store and client maker, for tests.
    pub fn new(shared: SharedStore, client: impl Fn() -> std::result::Result<AiClient, String> + Send + Sync + 'static) -> Self {
        Self { shared, client: Arc::new(client) }
    }

    /// The shared store.
    pub fn shared(&self) -> &SharedStore {
        &self.shared
    }

    /// A new client for this app.
    pub fn client(&self) -> std::result::Result<AiClient, String> {
        (self.client)()
    }
}

/// What the session keeps of the AI layer, behind locks so a `&Session` (the `enabled` checks) can
/// create it lazily.
#[derive(Default)]
pub struct AiState {
    /// Replaces the tool when set (tests).
    pub estimator: Option<Estimator>,
    host: Mutex<Option<AiHost>>,
    tools: Mutex<Option<Arc<AppTools>>>,
    status: Mutex<Option<(web_time::Instant, Option<String>)>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AiState {
    /// The host the frontend injected. A session without one (tests, embedders) has no AI layer:
    /// nothing reads a person's AI settings unless a frontend asks for it with
    /// [`use_standard_host`](Self::use_standard_host).
    pub fn host(&self) -> std::result::Result<AiHost, String> {
        lock(&self.host).clone().ok_or_else(|| "Estimate Depth is not set up in this frontend".to_owned())
    }

    /// Uses the person's real shared AI store (`CRAFT_AI_DIR` moves it). The desktop app and the
    /// CLI call this; the error is the sentence to show (no home folder).
    pub fn use_standard_host(&self) -> std::result::Result<(), String> {
        self.set_host(AiHost::standard()?);
        Ok(())
    }

    /// Uses `host` instead of the real store (tests). Forgets the tools and the cached status.
    pub fn set_host(&self, host: AiHost) {
        *lock(&self.host) = Some(host);
        *lock(&self.tools) = None;
        self.forget_status();
    }

    /// The tools view over the host, made on first use. A failure is not kept, so a later call
    /// tries again.
    pub fn tools(&self) -> std::result::Result<Arc<AppTools>, String> {
        let mut slot = lock(&self.tools);
        if let Some(tools) = slot.as_ref() {
            return Ok(tools.clone());
        }
        let tools = Arc::new(AppTools::with_client(self.host()?.client()?));
        *slot = Some(tools.clone());
        Ok(tools)
    }

    /// Why Estimate Depth cannot run now, in words for the person, or `None` when it can. Reused
    /// for a second; call [`forget_status`](Self::forget_status) after setup to see a change at once.
    pub fn status_reason(&self) -> Option<String> {
        if self.estimator.is_some() {
            return None;
        }
        let now = web_time::Instant::now();
        if let Some((at, reason)) = lock(&self.status).as_ref()
            && now.duration_since(*at) < STATUS_TTL
        {
            return reason.clone();
        }
        let reason = match self.tools() {
            Err(e) => Some(e),
            Ok(tools) => match tools.status(TOOL) {
                RoleStatus::Ready(_) => None,
                RoleStatus::Unavailable { reason } => Some(reason),
            },
        };
        *lock(&self.status) = Some((now, reason.clone()));
        reason
    }

    /// Drops the cached status.
    pub fn forget_status(&self) {
        *lock(&self.status) = None;
    }

    /// What a job calls: send `png` (`w` x `h`) and get the gray PNG back.
    pub(super) fn backend(&self) -> std::result::Result<Backend, String> {
        if let Some(e) = self.estimator.clone() {
            return Ok(Arc::new(move |_, png, w, h, q| e(&png, w, h, q.name()).map_err(bad)));
        }
        let tools = self.tools()?;
        Ok(Arc::new(move |ctx, png, _, _, q| call_tool(&tools, ctx, png, q)))
    }
}

/// The estimate step of a job: PNG in, gray PNG out.
pub(super) type Backend = Arc<dyn Fn(&JobCtx, Vec<u8>, u32, u32, Quality) -> Result<Vec<u8>> + Send + Sync>;

/// Run the tool: request, plan, run (waiting off to one side so the job's cancel reaches it).
fn call_tool(tools: &AppTools, ctx: &JobCtx, png: Vec<u8>, quality: Quality) -> Result<Vec<u8>> {
    let request = tools.request(TOOL).ok_or_else(|| bad("Estimate Depth is not available here"))?;
    let source = Payload::inline("image/png", png).map_err(|e| bad(e.message))?;
    let request = request.with_input(InputRole::Source, source).with_param("quality", json!(quality.name()));
    let plan = tools.plan(TOOL, request).map_err(bad)?;
    // A local model needs no consent. If the person's choice would send the picture away, this
    // command does not ask for them: that prompt belongs to the app's windows.
    let ledger = ConsentLedger::default();
    if plan.needs_consent(&ledger) {
        return Err(bad("The chosen model for Estimate Depth needs your agreement first. Use Estimate Depth from the brush settings in the app to review it."));
    }
    let canceller = tools.canceller();
    let outputs = std::thread::scope(|scope| {
        let worker = scope.spawn(|| tools.run_for(&plan, &ledger, LIMIT));
        while !worker.is_finished() {
            if ctx.cancelled() {
                // Asked again every tick: the job may not have been listed yet the first time.
                let _ = canceller.cancel();
            }
            std::thread::sleep(CANCEL_TICK);
        }
        worker.join().unwrap_or_else(|_| Err("Estimate Depth stopped unexpectedly".to_owned()))
    });
    if ctx.cancelled() {
        return Err(EngineError::Cancelled);
    }
    let outputs = outputs.map_err(bad)?;
    outputs
        .into_iter()
        .find_map(|o| match o.payload {
            Payload::Inline { media_type, data } if media_type.as_str() == "image/png" => Some(data),
            _ => None,
        })
        .ok_or_else(|| bad("Estimate Depth returned no picture"))
}

/// Encode interleaved 8-bit RGB as a PNG (size-checked against the layer's inline limit).
pub(super) fn encode_rgb8(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().map_err(|e| bad(format!("could not encode the picture: {e}")))?;
        writer.write_image_data(rgb).map_err(|e| bad(format!("could not encode the picture: {e}")))?;
    }
    if out.len() > MAX_SEND_BYTES {
        return Err(bad("the picture is too large to send for depth estimation"));
    }
    Ok(out)
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
