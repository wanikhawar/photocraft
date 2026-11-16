//! Stay on Object: the brush's Surface option that keeps a stroke on the object under its first
//! dab (see [`photocraft_paint::StayOn`]).
//!
//! The object is found once per stroke, from the picture the stroke samples (All Layers as seen,
//! the layer being painted included, or that layer alone), and the stroke
//! paints through it: the clip is the document's selection intersected with the object's region,
//! softened by `objectFeather`, and (with `ObjectAndDepth`) cut to the part reached from the first
//! dab without crossing a jump in depth. The renderers are unchanged: strokes already multiply their
//! coverage by the selection.
//!
//! Two finders produce the region:
//! * the shared AI layer's `select-subject` tool, with one positive click at the first dab, when it
//!   is set up and needs no consent (see [`finder`]); a run that fails is an error, never a quiet
//!   switch to the other finder;
//! * the built-in Object Finder ([`crate::smartselect_cmds::find_object`]) otherwise, and on the
//!   web build.
//!
//! The lock resolves lazily ([`ObjectLock::resolve`]) so the live stroke can do it on its paint
//! thread, and finds each object once: later strokes on it (the stroke's own paint does not change
//! an All Layers sample) hit the session's cache.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use photocraft_algo::selection::{self, Region, SelectionMode};
use photocraft_doc::{Document, LayerId};
use photocraft_geom::Rect;
use photocraft_paint::SampleMode;
use photocraft_paint::SurfaceOptions;
use photocraft_raster::Surface;

use super::surface_wire::{MAX_SAMPLE_PIXELS, same_sources};
use crate::{EngineError, Result, Session};

#[cfg(not(target_arch = "wasm32"))]
pub use crate::ai_layer::{Segmented, Segmenter};

/// Longest side of the picture sent to the segmenter (it resizes it again to its own size).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const SEND_SIDE: usize = 2048;
/// Objects one session remembers.
const KEEP_OBJECTS: usize = 4;
/// Rows of the canvas read at a time while sampling.
#[cfg(not(target_arch = "wasm32"))]
const BAND_ROWS: usize = 256;
/// The longest a stroke waits for the tool.
#[cfg(not(target_arch = "wasm32"))]
const TOOL_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);
/// Said when nothing is under the brush.
const NOT_FOUND: &str = "no object found under the brush";

fn fail(msg: impl Into<String>) -> EngineError {
    EngineError::Other(msg.into())
}

/// Which finder Stay on Object runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finder {
    /// The shared AI layer's Select Subject tool.
    Ai,
    /// The built-in Object Finder.
    BuiltIn,
}

/// Which finder a stroke would run now. Never blocks: while the first status of the tool is still
/// being read it says [`Finder::BuiltIn`]. A stroke itself waits for that first read instead.
pub fn finder(s: &Session) -> Finder {
    match ai_status(s) {
        None => Finder::Ai,
        Some(_) => Finder::BuiltIn,
    }
}

/// Why the AI object finder (the shared Select Subject tool) cannot run now, or `None` when it
/// can. Never blocks (see [`finder`]). The sentence is the shared layer's, for the Set up AI flow.
pub fn ai_status(s: &Session) -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    return s.ai.status_reason(crate::ai_layer::SELECT_SUBJECT_TOOL);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = s;
        Some("the AI object finder is not available in the browser".to_owned())
    }
}

/// How an object was found, so a result from one finder is not reused for the other.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Route {
    Ai,
    BuiltIn,
}

/// What finds the object.
#[derive(Clone)]
enum Backend {
    BuiltIn,
    #[cfg(not(target_arch = "wasm32"))]
    StandIn(Segmenter),
    #[cfg(not(target_arch = "wasm32"))]
    Ai(Arc<craft_ai_client::AppTools>),
}

impl Backend {
    fn route(&self) -> Route {
        match self {
            Backend::BuiltIn => Route::BuiltIn,
            #[cfg(not(target_arch = "wasm32"))]
            Backend::StandIn(_) | Backend::Ai(_) => Route::Ai,
        }
    }

    /// The finder the session would use now. Waits for the first status read, so the decision is
    /// the same each time it is asked.
    fn choose(s: &Session) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(seg) = s.ai.segmenter.clone() {
                return Backend::StandIn(seg);
            }
            if s.ai.known_status_reason(crate::ai_layer::SELECT_SUBJECT_TOOL).is_none()
                && let Ok(tools) = s.ai.tools()
            {
                return Backend::Ai(tools);
            }
        }
        let _ = s;
        Backend::BuiltIn
    }
}

/// An object found, with what it was found from: a lookup hits while the sampled pixels are the
/// same (see [`same_sources`]) and the click is inside it.
struct ObjectEntry {
    doc: Arc<Document>,
    sample: SampleMode,
    layer: Option<LayerId>,
    route: Route,
    region: Arc<Region>,
}

/// The objects a session has found (see the module doc), shared with the locks that use them.
#[derive(Default)]
pub struct ObjectCache {
    entries: Mutex<Vec<ObjectEntry>>,
    runs: AtomicUsize,
}

impl ObjectCache {
    /// How many times a finder has run (a cache hit runs none).
    pub fn runs(&self) -> usize {
        self.runs.load(Ordering::Relaxed)
    }

    fn lookup(&self, lock: &ObjectLock, x: i32, y: i32) -> Option<Arc<Region>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .iter()
            .find(|e| {
                e.route == lock.backend.route()
                    && e.sample == lock.sample
                    && e.layer == lock.layer
                    && e.region.at(x, y) >= lock.confidence
                    && same_sources(&e.doc, &lock.doc, lock.sample, lock.layer)
            })
            .map(|e| e.region.clone())
    }

    fn insert(&self, lock: &ObjectLock, region: Arc<Region>) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.len() >= KEEP_OBJECTS {
            entries.remove(0);
        }
        entries.push(ObjectEntry { doc: lock.doc.clone(), sample: lock.sample, layer: lock.layer, route: lock.backend.route(), region });
    }
}

/// Keeps the paint to the part of the object reached from the first dab without crossing a jump in
/// depth: the object's surface may run far nearer or farther (an urn's rim and foot), but where
/// something lies in front of it or behind it (a leaf over the rim) the depth jumps, and the paint
/// stops there.
///
/// The fill runs on cells about the size of a pixel of the depth estimate (its model sees about
/// 512 pixels across, so its edges blur over a few of them at the canvas's size), and compares each
/// cell with the one `SPAN` cells back the way the fill came: a jump blurred over a few cells still
/// shows in full, while a smooth surface changes little over that distance.
struct DepthGate {
    surface: Surface,
    /// The largest depth change (in the channel's 0..1 values) over `SPAN` cells that is still the
    /// same surface.
    tolerance: f32,
}

impl DepthGate {
    /// Cells across the canvas's longer side (about the depth model's own resolution).
    const CELLS: u32 = 512;
    /// How far back the fill looks for a jump, in cells.
    const SPAN: usize = 4;

    /// The part of `object` reached from `(cx, cy)` without crossing a depth jump. Each cell's depth
    /// is the mean over the object's pixels in it; cells with none are not the object.
    fn apply(&self, object: &Region, cx: i32, cy: i32, canvas: Rect) -> Result<Region> {
        let bbox = object.bbox;
        let (bw, bh) = (bbox.width() as usize, bbox.height() as usize);
        if bw == 0 || bh == 0 || object.mask.len() != bw.saturating_mul(bh) || !bbox.contains(cx, cy) {
            return Err(fail(NOT_FOUND));
        }
        let cell = (canvas.width().max(canvas.height()) / Self::CELLS).max(1) as usize;
        let (gw, gh) = (bw.div_ceil(cell), bh.div_ceil(cell));
        let channels = self.surface.channels().max(1);
        let depth = crate::allocation::read_region(&self.surface, bbox, "the depth channel")?;
        let mut sum = vec![0.0f32; gw * gh];
        let mut count = vec![0u32; gw * gh];
        for (i, v) in object.mask.iter().enumerate() {
            let d = depth.get(i * channels).copied().unwrap_or(f32::NAN);
            if *v >= SELECTED && d.is_finite() {
                let g = (i / bw / cell) * gw + (i % bw / cell);
                if let (Some(s), Some(n)) = (sum.get_mut(g), count.get_mut(g)) {
                    *s += d;
                    *n += 1;
                }
            }
        }
        let mean: Vec<Option<f32>> = sum.iter().zip(&count).map(|(s, n)| (*n > 0).then(|| s / *n as f32)).collect();
        let start = ((cy - bbox.y0) as usize / cell) * gw + (cx - bbox.x0) as usize / cell;
        if mean.get(start).copied().flatten().is_none() {
            return Err(fail(NOT_FOUND));
        }
        let mut reached = vec![false; gw * gh];
        let mut queue = std::collections::VecDeque::from([start]);
        if let Some(r) = reached.get_mut(start) {
            *r = true;
        }
        let tol = self.tolerance;
        while let Some(c) = queue.pop_front() {
            let (x, y) = (c % gw, c / gw);
            for (dx, dy) in [(-1isize, 0isize), (1, 0), (0, -1), (0, 1)] {
                let (Some(nx), Some(ny)) = (x.checked_add_signed(dx), y.checked_add_signed(dy)) else { continue };
                if nx >= gw || ny >= gh {
                    continue;
                }
                let n = ny * gw + nx;
                if reached.get(n).copied().unwrap_or(true) {
                    continue;
                }
                let Some(dn) = mean.get(n).copied().flatten() else { continue };
                // The reached cell farthest back the way the fill came, up to `SPAN` cells.
                let back = (1..Self::SPAN)
                    .rev()
                    .find_map(|k| {
                        let bx = x.checked_add_signed(-dx * k as isize)?;
                        let by = y.checked_add_signed(-dy * k as isize)?;
                        let b = by.checked_mul(gw)?.checked_add(bx)?;
                        (bx < gw && by < gh && reached.get(b).copied().unwrap_or(false)).then_some(b)
                    })
                    .unwrap_or(c);
                let Some(db) = mean.get(back).copied().flatten() else { continue };
                if (dn - db).abs() <= tol {
                    if let Some(r) = reached.get_mut(n) {
                        *r = true;
                    }
                    queue.push_back(n);
                }
            }
        }
        let mask = object
            .mask
            .iter()
            .enumerate()
            .map(|(i, v)| if reached.get((i / bw / cell) * gw + (i % bw / cell)).copied().unwrap_or(false) { *v } else { 0 })
            .collect();
        Ok(Region { bbox, mask })
    }
}

/// What a stroke needs to stay on its object, built from cheap inputs when the stroke starts and
/// resolved on first use (see the module doc). `Send + Sync`: a live stroke resolves it on its paint
/// thread.
pub(crate) struct ObjectLock {
    doc: Arc<Document>,
    sample: SampleMode,
    layer: Option<LayerId>,
    feather: f32,
    /// How sure the finder must be of a pixel (`SurfaceOptions::object_confidence`).
    confidence: f32,
    backend: Backend,
    cache: Arc<ObjectCache>,
    depth: Option<DepthGate>,
    result: OnceLock<std::result::Result<Arc<Region>, String>>,
    /// Set when whoever started the stroke let go of it: a wait for the finder ends (see
    /// [`ObjectLock::cancel_handle`]).
    cancel: Arc<AtomicBool>,
}

impl ObjectLock {
    /// The lock for a stroke aimed at `layer` on the active document with `opts` (Stay on Object on).
    /// Errors: no document, `ObjectAndDepth` without the depth channel (the words Follow Depth
    /// says), a canvas too large to sample.
    pub(crate) fn build(s: &Session, cmd: &str, opts: &SurfaceOptions, layer: Option<LayerId>) -> Result<Self> {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let canvas = st.doc.bounds();
        if u64::from(canvas.width()) * u64::from(canvas.height()) > MAX_SAMPLE_PIXELS {
            return Err(EngineError::BadParams { cmd: cmd.into(), msg: format!("Stay on Object needs a canvas of at most {MAX_SAMPLE_PIXELS} pixels") });
        }
        let depth = if opts.stay_on.uses_depth() {
            let channel = st
                .doc
                .channels
                .iter()
                .find(|c| c.name == opts.depth_channel)
                .ok_or_else(|| EngineError::BadParams { cmd: cmd.into(), msg: format!("no {} channel: run Image ▸ Estimate Depth", opts.depth_channel) })?;
            Some(DepthGate { surface: channel.surface.clone(), tolerance: opts.depth_tolerance })
        } else {
            None
        };
        Ok(Self {
            doc: st.doc.clone(),
            sample: opts.sample,
            layer,
            feather: opts.object_feather,
            confidence: opts.object_confidence,
            backend: Backend::choose(s),
            cache: s.surface_cache.objects.clone(),
            depth,
            result: OnceLock::new(),
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The flag that stops a pending [`resolve`](Self::resolve): once set, the finder wait ends and
    /// `resolve` gives [`EngineError::Cancelled`]. Shared, so it can be set while another thread is
    /// inside `resolve`.
    pub(crate) fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// The region the stroke paints in, for a stroke whose first dab is at `click` (document
    /// pixels). Computed once; later calls give the same answer (or error). May take a while (the
    /// finder runs): call it off the UI thread where a person is waiting.
    pub(crate) fn resolve(&self, click: (f64, f64)) -> Result<Arc<Region>> {
        if self.cancelled() {
            return Err(EngineError::Cancelled);
        }
        let result = self.result.get_or_init(|| self.compute(click).map_err(|e| e.to_string())).clone();
        result.map_err(|msg| if self.cancelled() { EngineError::Cancelled } else { EngineError::Other(msg) })
    }

    fn compute(&self, click: (f64, f64)) -> Result<Arc<Region>> {
        let canvas = self.doc.bounds();
        if !(click.0.is_finite() && click.1.is_finite()) {
            return Err(fail(NOT_FOUND));
        }
        let (cx, cy) = (click.0.floor() as i32, click.1.floor() as i32);
        if !canvas.contains(cx, cy) {
            return Err(fail(NOT_FOUND));
        }
        let object = match self.cache.lookup(self, cx, cy) {
            Some(hit) => hit,
            None => {
                let found = Arc::new(self.find(click, canvas)?);
                self.cache.runs.fetch_add(1, Ordering::Relaxed);
                self.cache.insert(self, found.clone());
                found
            }
        };
        // The cache keeps how sure the finder was; this stroke's confidence picks the pixels. The
        // depth cut comes before the feather, so its edge is softened too.
        let mut object = Arc::new(confident(&object, self.confidence));
        if let Some(gate) = &self.depth {
            object = Arc::new(gate.apply(&object, cx, cy, canvas)?);
        }
        let region = if self.feather > 0.0 { Arc::new(feathered(&object, self.feather, canvas)) } else { object };
        if region.mask.iter().all(|v| *v == 0) {
            return Err(fail(NOT_FOUND));
        }
        Ok(region)
    }

    /// Runs the finder for the object under `click`.
    #[cfg_attr(target_arch = "wasm32", allow(unused_variables))]
    fn find(&self, click: (f64, f64), canvas: Rect) -> Result<Region> {
        let all_layers = self.sample == SampleMode::AllLayers;
        // All Layers is the picture as it is seen, the layer being painted included: hiding it (as
        // Surface Color does) sent the finder a blank picture whenever that layer was the photo and
        // another layer showed anything (a white background). The cache ignores the painted layer
        // (`same_sources`), so the stroke's own paint never makes later strokes find again.
        let sampled: &Document = &self.doc;
        match &self.backend {
            Backend::BuiltIn => built_in(sampled, self.layer, all_layers, click),
            #[cfg(not(target_arch = "wasm32"))]
            Backend::StandIn(seg) => {
                let ask = |png: Vec<u8>, w: u32, h: u32, c: craft_ai_core::segment::Click| self.run_stand_in(seg, png, w, h, c);
                self.segment(sampled, click, canvas, &ask)?.map_or_else(|| built_in(sampled, self.layer, all_layers, click), Ok)
            }
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Ai(tools) => {
                let ask = |png: Vec<u8>, _: u32, _: u32, c: craft_ai_core::segment::Click| ask_tool(tools, png, c, &|| self.cancelled());
                // A model that would send the picture away without the person's agreement leaves the
                // built-in finder to do the work.
                self.segment(sampled, click, canvas, &ask)?.map_or_else(|| built_in(sampled, self.layer, all_layers, click), Ok)
            }
        }
    }

    /// Runs the stand-in segmenter on a thread of its own and waits for it, so a cancel ends the
    /// wait at once (the thread, if it is still inside the stand-in, finishes unseen).
    #[cfg(not(target_arch = "wasm32"))]
    fn run_stand_in(&self, seg: &Segmenter, png: Vec<u8>, w: u32, h: u32, click: craft_ai_core::segment::Click) -> Result<Option<Segmented>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let seg = seg.clone();
        std::thread::Builder::new()
            .name("stand-in segmenter".into())
            .spawn(move || {
                let _ = tx.send(seg(&png, w, h, &[click]));
            })
            .map_err(|_| fail("could not start the object finder"))?;
        let started = std::time::Instant::now();
        loop {
            if self.cancelled() {
                return Err(EngineError::Cancelled);
            }
            match rx.recv_timeout(std::time::Duration::from_millis(10)) {
                Ok(answer) => return answer.map(Some).map_err(fail),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if started.elapsed() < TOOL_LIMIT => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Err(fail("the object finder took too long")),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err(fail("the object finder stopped unexpectedly")),
            }
        }
    }

    /// Sends the sampled picture and one positive click to `ask`, and reads the mask back as a
    /// region over the canvas. `Ok(None)`: `ask` did not run the tool (consent).
    #[cfg(not(target_arch = "wasm32"))]
    fn segment(
        &self,
        sampled: &Document,
        click: (f64, f64),
        canvas: Rect,
        ask: &dyn Fn(Vec<u8>, u32, u32, craft_ai_core::segment::Click) -> Result<Option<Segmented>>,
    ) -> Result<Option<Region>> {
        let all_layers = self.sample == SampleMode::AllLayers;
        let (rgb, w, h) = sample_rgb(sampled, self.layer, all_layers, SEND_SIDE)?;
        let png = crate::ai_layer::encode_rgb8(&rgb, w, h)?;
        drop(rgb);
        let (cw, ch) = (f64::from(canvas.width()), f64::from(canvas.height()));
        let fraction = |v: f64, origin: i32, size: f64| (((v - f64::from(origin)) / size) as f32).clamp(0.0, 1.0);
        let click = craft_ai_core::segment::Click { x: fraction(click.0, canvas.x0, cw), y: fraction(click.1, canvas.y0, ch), positive: true };
        let Some(answer) = ask(png, w, h, click)? else { return Ok(None) };
        if !answer.found.matched {
            return Err(fail(NOT_FOUND));
        }
        let mask = decode_mask(&answer.mask, w, h)?;
        region_from_mask(&mask, w as usize, h as usize, canvas).map(Some).ok_or_else(|| fail(NOT_FOUND))
    }
}

/// One Select Subject run on the shared layer: `png` and a click in, the mask and what was found out.
#[cfg(not(target_arch = "wasm32"))]
fn ask_tool(
    tools: &craft_ai_client::AppTools,
    png: Vec<u8>,
    click: craft_ai_core::segment::Click,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Option<Segmented>> {
    use craft_ai_core::segment::{self, Found};
    use craft_ai_core::{InputRole, Payload};
    let tool = crate::ai_layer::SELECT_SUBJECT_TOOL;
    let request = tools.request(tool).ok_or_else(|| fail("Select Subject is not available here"))?;
    let source = Payload::inline("image/png", png).map_err(|e| fail(e.message))?;
    let request = segment::with_clicks(request.with_input(InputRole::Source, source), &[click]);
    let Some(outputs) = crate::ai_layer::call_tool(tools, tool, request, cancelled, TOOL_LIMIT)? else { return Ok(None) };
    let mask = match segment::mask(&outputs) {
        Some(Payload::Inline { data, .. }) => data.clone(),
        _ => return Err(fail("Select Subject returned no mask")),
    };
    let found = segment::read_found(&outputs).unwrap_or(Found { matched: true, score: None, name: None });
    Ok(Some(Segmented { mask, found }))
}

fn built_in(sampled: &Document, layer: Option<LayerId>, all_layers: bool, click: (f64, f64)) -> Result<Region> {
    crate::smartselect_cmds::find_object(sampled, layer, all_layers, click.0 as f32, click.1 as f32)
        .filter(|r| r.mask.iter().any(|v| *v > 0))
        .ok_or_else(|| fail(NOT_FOUND))
}

/// The longest-side-limited size of a `w` x `h` picture.
#[cfg(not(target_arch = "wasm32"))]
fn fit(w: usize, h: usize, max_side: usize) -> (usize, usize) {
    let longest = w.max(h);
    if longest <= max_side {
        return (w, h);
    }
    let scale = |v: usize| ((v as u128 * max_side as u128).div_ceil(longest as u128) as usize).max(1);
    (scale(w), scale(h))
}

/// The picture a sample shows as 8-bit RGB over white, at most `max_side` on its longer side
/// (box-averaged), read in bands so only the reduced picture is ever held.
#[cfg(not(target_arch = "wasm32"))]
fn sample_rgb(doc: &Document, layer: Option<LayerId>, all_layers: bool, max_side: usize) -> Result<(Vec<u8>, u32, u32)> {
    let canvas = doc.bounds();
    let (cw, ch) = (canvas.width() as usize, canvas.height() as usize);
    if cw == 0 || ch == 0 || (cw as u64).saturating_mul(ch as u64) > MAX_SAMPLE_PIXELS {
        return Err(fail("the picture is empty or too large to look at"));
    }
    let (w, h) = fit(cw, ch, max_side);
    let mut sums = vec![0.0f32; w * h * 3];
    let mut counts = vec![0u32; w * h];
    crate::smartselect_cmds::with_doc_sampler(doc, layer, all_layers, |smp, _| -> Result<()> {
        for y0 in (0..ch).step_by(BAND_ROWS) {
            let y1 = (y0 + BAND_ROWS).min(ch);
            let band = Rect::new(canvas.x0, canvas.y0.saturating_add(y0 as i32), canvas.x1, canvas.y0.saturating_add(y1 as i32));
            let px = smp.rgba(band);
            if px.len() != cw * (y1 - y0) {
                return Err(fail("the picture could not be read"));
            }
            for (i, p) in px.iter().enumerate() {
                let (dx, dy) = ((i % cw) * w / cw, (y0 + i / cw) * h / ch);
                let a = if p[3].is_finite() { p[3].clamp(0.0, 1.0) } else { 1.0 };
                let cell = dy * w + dx;
                if let (Some(sum), Some(n)) = (sums.get_mut(cell * 3..cell * 3 + 3), counts.get_mut(cell)) {
                    for (c, v) in sum.iter_mut().zip(p) {
                        *c += if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 } * a + (1.0 - a);
                    }
                    *n += 1;
                }
            }
        }
        Ok(())
    })?;
    let rgb = sums
        .as_chunks::<3>()
        .0
        .iter()
        .zip(&counts)
        .flat_map(|(sum, n)| sum.iter().map(move |v| if *n == 0 { 255 } else { (v / *n as f32 * 255.0 + 0.5).clamp(0.0, 255.0) as u8 }))
        .collect();
    Ok((rgb, w as u32, h as u32))
}

/// Decodes the tool's gray mask PNG (8- or 16-bit) to bytes. Its size must be the one sent.
#[cfg(not(target_arch = "wasm32"))]
fn decode_mask(bytes: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let invalid = || fail("Select Subject returned a mask that could not be read");
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().map_err(|_| invalid())?;
    let info = reader.info();
    let (iw, ih, color, depth) = (info.width, info.height, info.color_type, info.bit_depth);
    if (iw, ih) != (w, h) {
        return Err(fail(format!("Select Subject returned a {iw} x {ih} mask for a {w} x {h} picture")));
    }
    let wide = match (color, depth) {
        (png::ColorType::Grayscale, png::BitDepth::Eight) => false,
        (png::ColorType::Grayscale, png::BitDepth::Sixteen) => true,
        _ => return Err(fail("Select Subject returned a mask that is not 8- or 16-bit gray")),
    };
    let size = reader.output_buffer_size().ok_or_else(invalid)?;
    let expected = (iw as usize).saturating_mul(ih as usize).saturating_mul(if wide { 2 } else { 1 });
    if size != expected || expected > 2 * MAX_SAMPLE_PIXELS as usize {
        return Err(invalid());
    }
    let mut buf = vec![0u8; size];
    reader.next_frame(&mut buf).map_err(|_| invalid())?;
    Ok(if wide { buf.as_chunks::<2>().0.iter().map(|p| p[0]).collect() } else { buf })
}

/// A pixel of a [`confident`] region that is the object.
const SELECTED: u8 = 128;
/// The least mask value [`region_from_mask`] keeps: just under the lowest confidence a stroke may ask
/// for (`OBJECT_CONFIDENCE_RANGE`), so the region's box is not the whole canvas when the model is
/// all but sure the rest is not the object.
#[cfg(not(target_arch = "wasm32"))]
const LEAST_KEPT: u8 = 2;

/// The pixels of `object` the finder is at least `confidence` (0..1) sure of, fully in; the rest out.
/// The finder's mask is how sure it is that each pixel is the object (SAM's coverage), not how much
/// of it to paint: taken as coverage, the parts it was less sure of (a rim, a foot in shadow) painted
/// faded, and the rest of the picture took a faint coat. The Feather option softens the edge.
fn confident(object: &Region, confidence: f32) -> Region {
    let at_least = (confidence.clamp(0.0, 1.0) * 255.0).round().max(1.0);
    Region { bbox: object.bbox, mask: object.mask.iter().map(|v| if f32::from(*v) >= at_least { 255 } else { 0 }).collect() }
}

#[cfg(not(target_arch = "wasm32"))]
/// A `mw` x `mh` mask stretched over `canvas` (bilinear) as a region over the box of the pixels the
/// model gives at least [`LEAST_KEPT`]; `None` when there are none. The values stay how sure the
/// model is: each stroke picks its pixels with [`confident`].
fn region_from_mask(mask: &[u8], mw: usize, mh: usize, canvas: Rect) -> Option<Region> {
    let (cw, ch) = (canvas.width() as usize, canvas.height() as usize);
    if mw == 0 || mh == 0 || cw == 0 || ch == 0 || mask.len() != mw.checked_mul(mh)? {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (i, v) in mask.iter().enumerate() {
        if *v >= LEAST_KEPT {
            let (x, y) = (i % mw, i / mw);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    if x0 == usize::MAX {
        return None;
    }
    // The canvas pixels the box covers, one more each side for the bilinear fringe.
    let to_canvas = |v: usize, from: usize, to: usize| v.saturating_mul(to) / from;
    let (bx0, by0) = (to_canvas(x0, mw, cw).saturating_sub(1), to_canvas(y0, mh, ch).saturating_sub(1));
    let (bx1, by1) = ((to_canvas(x1 + 1, mw, cw) + 2).min(cw), (to_canvas(y1 + 1, mh, ch) + 2).min(ch));
    let (bw, bh) = (bx1.checked_sub(bx0)?, by1.checked_sub(by0)?);
    let at = |x: usize, y: usize| f32::from(mask.get(y.min(mh - 1) * mw + x.min(mw - 1)).copied().unwrap_or(0));
    let mut out = Vec::with_capacity(bw.checked_mul(bh)?);
    for py in by0..by1 {
        let v = ((py as f32 + 0.5) * mh as f32 / ch as f32 - 0.5).clamp(0.0, (mh - 1) as f32);
        let (fy, ty) = (v.floor() as usize, v.fract());
        for px in bx0..bx1 {
            let u = ((px as f32 + 0.5) * mw as f32 / cw as f32 - 0.5).clamp(0.0, (mw - 1) as f32);
            let (fx, tx) = (u.floor() as usize, u.fract());
            let top = at(fx, fy) * (1.0 - tx) + at(fx + 1, fy) * tx;
            let bottom = at(fx, fy + 1) * (1.0 - tx) + at(fx + 1, fy + 1) * tx;
            out.push((top * (1.0 - ty) + bottom * ty + 0.5).clamp(0.0, 255.0) as u8);
        }
    }
    let bbox = Rect::new(
        canvas.x0.checked_add(bx0 as i32)?,
        canvas.y0.checked_add(by0 as i32)?,
        canvas.x0.checked_add(bx1 as i32)?,
        canvas.y0.checked_add(by1 as i32)?,
    );
    Some(Region { bbox, mask: out })
}

/// `region` with its edge softened over about `radius` pixels (the box grows by the reach of the
/// blur, inside `canvas`).
fn feathered(region: &Region, radius: f32, canvas: Rect) -> Region {
    let reach = i32::try_from(selection::feather_reach(radius)).unwrap_or(i32::MAX);
    let b = region.bbox;
    let window = Rect::new(b.x0.saturating_sub(reach), b.y0.saturating_sub(reach), b.x1.saturating_add(reach), b.y1.saturating_add(reach)).intersect(&canvas);
    let (w, h) = (window.width() as usize, window.height() as usize);
    if w == 0 || h == 0 {
        return region.clone();
    }
    let mut plane = vec![0.0f32; w * h];
    let bw = b.width() as usize;
    for (i, v) in region.mask.iter().enumerate() {
        let (x, y) = (b.x0 + (i % bw.max(1)) as i32, b.y0 + (i / bw.max(1)) as i32);
        if window.contains(x, y)
            && let Some(cell) = plane.get_mut((y - window.y0) as usize * w + (x - window.x0) as usize)
        {
            *cell = f32::from(*v) / 255.0;
        }
    }
    let blurred = selection::feather(&plane, w, h, radius);
    if blurred.len() != w * h {
        return region.clone();
    }
    Region { bbox: window, mask: blurred.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect() }
}

/// The selection a stroke paints through: `sel` (the document's, if any) intersected with `region`.
/// Never `None`: a region that misses the selection entirely leaves an empty selection, which
/// paints nothing.
pub(crate) fn clip_selection(sel: Option<&Surface>, region: &Region) -> Surface {
    let mode = if sel.is_some() { SelectionMode::Intersect } else { SelectionMode::Replace };
    selection::combine_region(sel, Some(region), mode).unwrap_or_else(|| Surface::new(photocraft_color::PixelFormat::GRAY8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(bbox: Rect, f: impl Fn(i32, i32) -> u8) -> Region {
        let mask = (bbox.y0..bbox.y1).flat_map(|y| (bbox.x0..bbox.x1).map(move |x| (x, y))).map(|(x, y)| f(x, y)).collect();
        Region { bbox, mask }
    }

    /// The feathered edge is smooth: along a row it never decreases going inward, stays between 0
    /// and 1, is about half at the old edge, and the box grew to hold the fade.
    #[test]
    fn a_feathered_edge_is_monotone_and_bounded() {
        let canvas = Rect::new(0, 0, 120, 60);
        let object = region(Rect::new(40, 10, 80, 50), |_, _| 255);
        let soft = feathered(&object, 4.0, canvas);
        assert!(soft.bbox.contains_rect(&object.bbox) && soft.bbox.x0 < 40 && soft.bbox.x1 > 80);
        assert_eq!(soft.mask.len(), (soft.bbox.width() * soft.bbox.height()) as usize);
        let row: Vec<f32> = (soft.bbox.x0..soft.bbox.x1).map(|x| soft.at(x, 30)).collect();
        assert!(row.iter().all(|v| (0.0..=1.0).contains(v)));
        let peak = row.len() / 2;
        assert!(row[..=peak].windows(2).all(|w| w[0] <= w[1] + 1e-6), "rising into the object: {row:?}");
        assert!(row[peak..].windows(2).all(|w| w[0] + 1e-6 >= w[1]), "falling out of it: {row:?}");
        let edge = soft.at(40, 30);
        assert!((0.25..0.75).contains(&edge), "about half at the old edge: {edge}");
        assert!(soft.at(60, 30) > 0.97 && soft.at(30, 30) < 0.2 && soft.at(10, 30) == 0.0);
        // No feather is no change in the rest of the code path; a box at the canvas edge stays inside it.
        let edge_box = feathered(&region(Rect::new(0, 0, 10, 10), |_, _| 255), 6.0, canvas);
        assert!(canvas.contains_rect(&edge_box.bbox));
    }

    /// A mask of another size than the sample, an empty one or a mismatched buffer is nothing, not a panic.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn hostile_masks_select_nothing() {
        let canvas = Rect::new(0, 0, 40, 20);
        assert!(region_from_mask(&[], 0, 0, canvas).is_none());
        assert!(region_from_mask(&[255; 10], 4, 4, canvas).is_none());
        assert!(region_from_mask(&[0; 16], 4, 4, canvas).is_none());
        assert!(region_from_mask(&[255; 16], 4, 4, Rect::EMPTY).is_none());
        // A mask over a canvas at an offset lands at the offset.
        let shifted = region_from_mask(&[255; 16], 4, 4, Rect::new(100, 50, 140, 70)).unwrap();
        assert!(shifted.bbox.x0 >= 100 && shifted.bbox.y0 >= 50 && shifted.bbox.x1 <= 140 && shifted.bbox.y1 <= 70);
        assert_eq!(shifted.at(120, 60), 1.0);
    }

    /// The tool's mask says how sure the model is, not how much to paint: a pixel it is at least the
    /// stroke's confidence sure of is painted in full, and the rest not at all (taken as coverage, a
    /// 60% sure rim painted at 60%, and the rest of the picture took a faint coat).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_confidence_picks_the_pixels_the_finder_is_sure_enough_of() {
        let mask = [153u8, 153, 102, 102, 1, 1];
        let r = region_from_mask(&mask, 6, 1, Rect::new(0, 0, 60, 10)).unwrap();
        assert!(r.bbox.x1 <= 42, "the box leaves out what the model is all but sure is not the object: {:?}", r.bbox);
        let half = confident(&r, 0.5);
        assert_eq!((half.at(5, 5), half.at(35, 5)), (1.0, 0.0), "more likely than not: the 60% pixel in, the 40% one out");
        assert!(half.mask.iter().all(|v| *v == 0 || *v == 255), "no faded coverage");
        let low = confident(&r, 0.3);
        assert_eq!((low.at(5, 5), low.at(35, 5)), (1.0, 1.0), "a low confidence takes the 40% pixel too");
        assert_eq!(confident(&r, 0.7).at(5, 5), 0.0, "a high one leaves the 60% pixel out");
        assert!(region_from_mask(&[1; 4], 2, 2, Rect::new(0, 0, 4, 4)).is_none(), "nothing the model gives a chance");
    }

    /// A mask smaller than the canvas is stretched over it, with the object where it was.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_reduced_mask_is_stretched_back_over_the_canvas() {
        let mask: Vec<u8> = (0..8 * 4).map(|i| if (2..6).contains(&(i % 8)) { 255 } else { 0 }).collect();
        let r = region_from_mask(&mask, 8, 4, Rect::new(0, 0, 80, 40)).unwrap();
        assert!(r.at(40, 20) == 1.0 && r.at(5, 20) == 0.0 && r.at(75, 20) == 0.0);
        assert!(r.at(19, 20) < 0.5 && r.at(26, 20) > 0.5, "the edge stays near 20 px: {} {}", r.at(19, 20), r.at(26, 20));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn fit_keeps_the_aspect_and_never_gives_zero() {
        assert_eq!(fit(100, 50, 2048), (100, 50));
        assert_eq!(fit(4096, 2048, 2048), (2048, 1024));
        assert_eq!(fit(10_000, 1, 2048), (2048, 1));
        assert_eq!(fit(1, 10_000, 2048), (1, 2048));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_mask_of_the_wrong_size_or_kind_is_an_error() {
        let png = |w: u32, h: u32, color: png::ColorType, depth: png::BitDepth, data: &[u8]| {
            let mut out = Vec::new();
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(color);
            enc.set_depth(depth);
            enc.write_header().unwrap().write_image_data(data).unwrap();
            out
        };
        let ok = png(2, 2, png::ColorType::Grayscale, png::BitDepth::Eight, &[0, 255, 255, 0]);
        assert_eq!(decode_mask(&ok, 2, 2).unwrap(), vec![0, 255, 255, 0]);
        assert!(decode_mask(&ok, 3, 2).is_err());
        assert!(decode_mask(b"not a png", 2, 2).is_err());
        let rgb = png(1, 1, png::ColorType::Rgb, png::BitDepth::Eight, &[1, 2, 3]);
        assert!(decode_mask(&rgb, 1, 1).is_err());
        let wide = png(1, 2, png::ColorType::Grayscale, png::BitDepth::Sixteen, &[255, 0, 0, 255]);
        assert_eq!(decode_mask(&wide, 1, 2).unwrap(), vec![255, 0]);
    }

    /// A selection that the object misses leaves an empty one, never none (which paints everywhere).
    #[test]
    fn clipping_to_a_disjoint_selection_leaves_nothing() {
        let object = region(Rect::new(10, 10, 20, 20), |_, _| 255);
        let clip = clip_selection(None, &object);
        assert!(clip.content_bounds().contains_rect(&Rect::new(10, 10, 20, 20)) || !clip.content_bounds().is_empty());
        let elsewhere = region(Rect::new(50, 50, 60, 60), |_, _| 255);
        let sel = selection::combine_region(None, Some(&elsewhere), SelectionMode::Replace).unwrap();
        let none = clip_selection(Some(&sel), &object);
        assert!(none.content_bounds().is_empty());
        let both = clip_selection(Some(&sel), &elsewhere);
        assert!(!both.content_bounds().is_empty());
    }
}
