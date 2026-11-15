//! The app's side of the shared Craft AI layer (storytold/craft-ai-integrations), for every tool
//! that uses it (Estimate Depth, Select Subject for the brush's Stay on Object).
//!
//! The models, their files, where they live and how they are set up belong to the shared layer.
//! This module asks the layer whether a tool can run (per tool, kept for a second), runs a tool
//! on a prepared request while the caller's cancel reaches it, and holds the stand-ins tests use
//! in place of a tool. Nothing here names a model, an engine or a file, and nothing is
//! downloaded: setup is the shared Set up AI flow (the launcher, or the app's own screen over
//! the same pieces). Native only: the web build has no tools.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use craft_ai_client::{AiClient, AppTools, Job, RoleStatus};
use craft_ai_core::{AppId, ConsentLedger, Event, JobState, Locality, Output, Request};
use craft_ai_native::shared::SharedStore;

use crate::{EngineError, Result};

/// The catalogue id of Estimate Depth.
pub const DEPTH_TOOL: &str = "estimate-depth";
/// The catalogue id of Select Subject (capability `image.segment`), which the brush's Stay on
/// Object runs for a click.
pub const SELECT_SUBJECT_TOOL: &str = "select-subject";
/// This app's id in the shared layer (consent and per-app choices are scoped by it).
const APP: &str = "photocraft";
/// The longest Estimate Depth may take before it is stopped.
pub(crate) const DEPTH_LIMIT: Duration = Duration::from_secs(10 * 60);
/// How often a job on this computer is looked at (an online one once a second).
const LOCAL_STEP: Duration = Duration::from_millis(15);
const REMOTE_STEP: Duration = Duration::from_secs(1);
/// How often a waiting job looks at its cancel flag.
pub(crate) const CANCEL_TICK: Duration = Duration::from_millis(30);
/// How long a status answer is reused, so enabling checks do not read the shared store each frame.
const STATUS_TTL: Duration = Duration::from_secs(1);
/// Largest picture sent to a tool, in bytes (the layer's inline limit is 32 MiB).
pub(crate) const MAX_SEND_BYTES: usize = 32 * 1024 * 1024 - 1;

/// A test hook that replaces Estimate Depth: it gets the PNG, its size and the quality name, and
/// returns a 16-bit (or 8-bit) gray PNG of the same size, white = near.
pub type Estimator = Arc<dyn Fn(&[u8], u32, u32, &str) -> std::result::Result<Vec<u8>, String> + Send + Sync>;

/// A test hook that replaces Select Subject: it gets the PNG, its size and the clicks (fractions
/// of the picture), and answers as the tool does.
pub type Segmenter = Arc<dyn Fn(&[u8], u32, u32, &[craft_ai_core::segment::Click]) -> std::result::Result<Segmented, String> + Send + Sync>;

/// What a Select Subject run answers: a gray mask PNG the size of the picture (white = selected)
/// and what was found.
#[derive(Clone, Debug)]
pub struct Segmented {
    pub mask: Vec<u8>,
    pub found: craft_ai_core::segment::Found,
}

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

/// What the session keeps of the AI layer. Shared behind an `Arc` so the background status refresh
/// can write its answer; the locks let a `&Session` (the `enabled` checks) create things lazily.
#[derive(Default)]
pub struct AiState {
    /// Replaces Estimate Depth when set (tests).
    pub estimator: Option<Estimator>,
    /// Replaces Select Subject when set (tests).
    pub segmenter: Option<Segmenter>,
    inner: Arc<Inner>,
}

/// One tool's cached status.
#[derive(Default)]
struct Slot {
    /// When the answer was read and what it was: `None` = the tool can run, else why not.
    answer: Option<(web_time::Instant, Option<String>)>,
    /// A background refresh of this tool is running (at most one at a time).
    refreshing: bool,
}

#[derive(Default)]
struct Inner {
    host: Mutex<Option<AiHost>>,
    /// Why the standard host could not be made (no home folder), kept so the commands say it
    /// instead of the generic "not set up in this frontend".
    host_error: Mutex<Option<String>>,
    tools: Mutex<Option<Arc<AppTools>>>,
    /// Each tool's cached status. The condition variable waits on this lock.
    slots: Mutex<HashMap<String, Slot>>,
    /// Bumped when the cached statuses are forgotten, so a refresh that began before is not stored.
    epoch: AtomicU64,
    /// Signalled (with the `slots` lock held) when a background refresh ends, so a reader that
    /// found no answer can wait for the read in flight instead of starting a second one.
    refresh_done: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The words for a tool in a sentence ("Checking Estimate Depth…").
fn label(tool: &str) -> &str {
    match tool {
        DEPTH_TOOL => "Estimate Depth",
        SELECT_SUBJECT_TOOL => "Select Subject",
        other => other,
    }
}

impl Inner {
    fn host(&self) -> std::result::Result<AiHost, String> {
        lock(&self.host).clone().ok_or_else(|| lock(&self.host_error).clone().unwrap_or_else(|| "AI is not set up in this frontend".to_owned()))
    }

    fn tools(&self) -> std::result::Result<Arc<AppTools>, String> {
        let mut slot = lock(&self.tools);
        if let Some(tools) = slot.as_ref() {
            return Ok(tools.clone());
        }
        let tools = Arc::new(AppTools::with_client(self.host()?.client()?));
        *slot = Some(tools.clone());
        Ok(tools)
    }

    /// Reads the shared store, the key store and the model files: slow, never on the UI thread
    /// per frame.
    fn compute(&self, tool: &str) -> Option<String> {
        match self.tools() {
            Err(e) => Some(e),
            Ok(tools) => match tools.status(tool) {
                RoleStatus::Ready(_) => None,
                RoleStatus::Unavailable { reason } => Some(reason),
            },
        }
    }

    /// The answer for a reader that found none cached: waits for the background refresh in flight
    /// (so there is one read, one answer), and reads now only when none is running. Never holds
    /// the status lock while reading.
    fn wait_or_refresh(&self, tool: &str) -> Option<String> {
        {
            let mut slots = lock(&self.slots);
            loop {
                let slot = slots.entry(tool.to_owned()).or_default();
                if let Some((_, reason)) = slot.answer.as_ref() {
                    return reason.clone();
                }
                if !slot.refreshing {
                    break;
                }
                slots = self.refresh_done.wait(slots).unwrap_or_else(PoisonError::into_inner);
            }
        }
        self.refresh(tool)
    }

    /// Marks `tool`'s background refresh as ended and wakes the readers waiting for it. Taking
    /// the lock before the wake-up means a waiter either sees the flag cleared or is already
    /// waiting.
    fn finish_refresh(&self, tool: &str) {
        let mut slots = lock(&self.slots);
        slots.entry(tool.to_owned()).or_default().refreshing = false;
        self.refresh_done.notify_all();
    }

    /// Computes `tool`'s status and keeps it, unless it was forgotten meanwhile.
    fn refresh(&self, tool: &str) -> Option<String> {
        let epoch = self.epoch.load(Ordering::SeqCst);
        let reason = self.compute(tool);
        let mut slots = lock(&self.slots);
        if self.epoch.load(Ordering::SeqCst) == epoch {
            slots.entry(tool.to_owned()).or_default().answer = Some((web_time::Instant::now(), reason.clone()));
        }
        reason
    }
}

impl AiState {
    /// The host the frontend injected. A session without one (tests, embedders) has no AI layer:
    /// nothing reads a person's AI settings unless a frontend asks for it with
    /// [`use_standard_host`](Self::use_standard_host).
    pub fn host(&self) -> std::result::Result<AiHost, String> {
        self.inner.host()
    }

    /// Uses the person's real shared AI store (`CRAFT_AI_DIR` moves it). The desktop app and the
    /// CLI call this; the error is the sentence to show (no home folder).
    pub fn use_standard_host(&self) -> std::result::Result<(), String> {
        match AiHost::standard() {
            Ok(host) => {
                self.set_host(host);
                Ok(())
            }
            Err(reason) => {
                self.fail_host(reason.clone());
                Err(reason)
            }
        }
    }

    /// Keeps `reason` as why there is no host, so [`host`](Self::host) and the status say it.
    pub(crate) fn fail_host(&self, reason: String) {
        *lock(&self.inner.host_error) = Some(reason);
        self.forget_status();
    }

    /// The same AI layer for another session: the host, the tools and the statuses are this one's
    /// (a scratch session of Batch or a droplet runs actions that estimate depth), and so are the
    /// test hooks. A session without a host gives one without a host, so tests stay isolated.
    pub fn shared(&self) -> AiState {
        AiState { estimator: self.estimator.clone(), segmenter: self.segmenter.clone(), inner: self.inner.clone() }
    }

    /// Uses `host` instead of the real store (tests). Forgets the tools and the cached statuses.
    pub fn set_host(&self, host: AiHost) {
        *lock(&self.inner.host) = Some(host);
        *lock(&self.inner.host_error) = None;
        *lock(&self.inner.tools) = None;
        self.forget_status();
    }

    /// The tools view over the host, made on first use. A failure is not kept, so a later call
    /// tries again.
    pub fn tools(&self) -> std::result::Result<Arc<AppTools>, String> {
        self.inner.tools()
    }

    /// Does a test hook stand in for `tool`? Such a tool always counts as ready.
    pub fn stands_in(&self, tool: &str) -> bool {
        match tool {
            DEPTH_TOOL => self.estimator.is_some(),
            SELECT_SUBJECT_TOOL => self.segmenter.is_some(),
            _ => false,
        }
    }

    /// Why `tool` cannot run now, in words for the person, or `None` when it can. Never blocks:
    /// it returns the last answer at once and, when that is older than a second, reads the store
    /// again on a background thread. Before the first answer it says the tool is being checked.
    /// For a decision that must be right (running the command), use
    /// [`status_reason_now`](Self::status_reason_now).
    pub fn status_reason(&self, tool: &str) -> Option<String> {
        if self.stands_in(tool) {
            return None;
        }
        let known = lock(&self.inner.slots).get(tool).and_then(|s| s.answer.clone());
        match known {
            Some((at, reason)) => {
                if web_time::Instant::now().duration_since(at) >= STATUS_TTL {
                    self.refresh_in_background(tool);
                }
                reason
            }
            None => {
                self.refresh_in_background(tool);
                Some(format!("Checking {}\u{2026}", label(tool)))
            }
        }
    }

    /// The answer for a command's `enabled` check. Once an answer is known it is returned at
    /// once (and refreshed in the background when older than a second). Setting a host (and
    /// forgetting the statuses) starts a background read, so the cache is normally warm by the
    /// first check. When none is known yet, this waits for the read in flight, or reads now when
    /// none is running, and returns the kept answer: reading `enabled` twice must give one answer
    /// (a listing of the commands read twice must not change), which a provisional "enabled"
    /// while a background read runs would not.
    pub fn known_status_reason(&self, tool: &str) -> Option<String> {
        if self.stands_in(tool) {
            return None;
        }
        let known = lock(&self.inner.slots).get(tool).and_then(|s| s.answer.clone());
        match known {
            Some((at, reason)) => {
                if web_time::Instant::now().duration_since(at) >= STATUS_TTL {
                    self.refresh_in_background(tool);
                }
                reason
            }
            None => self.inner.wait_or_refresh(tool),
        }
    }

    /// As [`status_reason`](Self::status_reason) but reads the store now, on this thread, and keeps
    /// the answer. For a person's action, not for a frame.
    pub fn status_reason_now(&self, tool: &str) -> Option<String> {
        if self.stands_in(tool) {
            return None;
        }
        self.inner.refresh(tool)
    }

    /// Starts one background refresh of `tool` unless one is already running.
    fn refresh_in_background(&self, tool: &str) {
        {
            let mut slots = lock(&self.inner.slots);
            let slot = slots.entry(tool.to_owned()).or_default();
            if slot.refreshing {
                return;
            }
            slot.refreshing = true;
        }
        let inner = self.inner.clone();
        let name = tool.to_owned();
        let started = std::thread::Builder::new().name("photocraft-ai-status".into()).spawn(move || {
            // A panic in the shared layer must not leave `refreshing` set for good.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.refresh(&name)));
            inner.finish_refresh(&name);
        });
        if started.is_err() {
            self.inner.finish_refresh(tool);
        }
    }

    /// Drops every cached status and starts reading again in the background for the tools asked
    /// about so far (and Estimate Depth, which every frontend asks about first), so the next check
    /// normally finds them warm.
    pub fn forget_status(&self) {
        self.inner.epoch.fetch_add(1, Ordering::SeqCst);
        let mut asked: Vec<String> = {
            let mut slots = lock(&self.inner.slots);
            for slot in slots.values_mut() {
                slot.answer = None;
            }
            slots.keys().cloned().collect()
        };
        if !asked.iter().any(|t| t == DEPTH_TOOL) {
            asked.push(DEPTH_TOOL.to_owned());
        }
        for tool in asked.iter().filter(|t| !self.stands_in(t)) {
            self.refresh_in_background(tool);
        }
    }
}

/// Runs `tool` on `request`: plan, run (waiting off to one side so `cancelled` reaches the job, and
/// giving up after `limit`), and return what it produced. `label` names the tool in a sentence. `Ok(None)` means the
/// person's chosen model would send the picture away and they have not agreed: nothing ran, and
/// the caller says so (or uses another route). Errors are the sentence to show.
pub(crate) fn call_tool(tools: &AppTools, tool: &str, request: Request, cancelled: &(dyn Fn() -> bool + Sync), limit: Duration) -> Result<Option<Vec<Output>>> {
    let words = label(tool);
    let plan = tools.plan(tool, request).map_err(bad)?;
    // A local model needs no consent. If the person's choice would send the picture away, this
    // command does not ask for them: that prompt belongs to the app's windows.
    let ledger = ConsentLedger::default();
    if plan.needs_consent(&ledger) {
        return Ok(None);
    }
    // This job's own handle: cancelling it must not stop another document's run, which the
    // session's shared tools may be waiting for at the same time.
    let job = tools.client().start(&plan, &ledger).map_err(|e| bad(e.message))?;
    let step = match plan.disclosure().locality {
        Locality::Remote => REMOTE_STEP,
        Locality::Local => LOCAL_STEP,
    };
    let done = wait_cancellable(cancelled, || job.cancel().is_ok(), || job.wait(step, limit))
        .unwrap_or_else(|| Err(craft_ai_core::AiError::invalid(format!("{words} stopped unexpectedly"))));
    if cancelled() {
        return Err(EngineError::Cancelled);
    }
    // A wait that ended without the job ending asks it to stop; say so when it would not.
    let done =
        done.map_err(|e| if stopped(&job) { bad(e.message) } else { bad(format!("{} It could not be stopped, so it may still be running.", e.message)) })?;
    match done.state {
        JobState::Succeeded => {}
        JobState::Failed { error } => return Err(bad(error.message)),
        JobState::Cancelled => return Err(EngineError::Cancelled),
        _ => return Err(bad("The job did not finish.")),
    }
    Ok(Some(
        done.events
            .into_iter()
            .filter_map(|e| match e {
                Event::Output(o) => Some(o),
                _ => None,
            })
            .collect(),
    ))
}

pub(crate) fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::Other(msg.into())
}

/// True when `job` stopped or had already ended after its wait gave up.
fn stopped(job: &Job) -> bool {
    match job.cancel() {
        Ok(()) => true,
        Err(_) => job.poll().is_ok_and(|snap| snap.state.is_terminal()),
    }
}

/// Runs `wait` on a worker thread and, while it runs, calls `cancel` every tick once `cancelled`
/// says so (asked again each tick: the job may not have been known to the service the first time).
/// `cancel` belongs to the one job `wait` waits for, never to a shared list of jobs. `None` when
/// the worker panicked.
pub(crate) fn wait_cancellable<T: Send>(cancelled: impl Fn() -> bool + Sync, cancel: impl Fn() -> bool + Sync, wait: impl FnOnce() -> T + Send) -> Option<T> {
    std::thread::scope(|scope| {
        let worker = scope.spawn(wait);
        while !worker.is_finished() {
            if cancelled() {
                let _ = cancel();
            }
            std::thread::sleep(CANCEL_TICK);
        }
        worker.join().ok()
    })
}

/// Encode interleaved 8-bit RGB as a PNG (size-checked against the layer's inline limit).
pub(crate) fn encode_rgb8(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
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
        return Err(bad("the picture is too large to send"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    /// A job as the wait sees it: it ends when released or when asked to stop, and counts the stops.
    struct Fake {
        release: AtomicBool,
        stop: AtomicBool,
        stops: AtomicUsize,
    }

    impl Fake {
        fn new() -> Self {
            Self { release: AtomicBool::new(false), stop: AtomicBool::new(false), stops: AtomicUsize::new(0) }
        }

        fn wait(&self) -> &'static str {
            loop {
                if self.stop.load(Ordering::SeqCst) {
                    return "stopped";
                }
                if self.release.load(Ordering::SeqCst) {
                    return "done";
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn cancel(&self) -> bool {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.stop.store(true, Ordering::SeqCst);
            true
        }
    }

    /// Cancelling one document's run stops only that one while another keeps running.
    #[test]
    fn cancelling_one_wait_leaves_a_concurrent_one_running() {
        let (a, b) = (Fake::new(), Fake::new());
        let (cancel_a, cancel_b) = (AtomicBool::new(false), AtomicBool::new(false));
        std::thread::scope(|scope| {
            let first = scope.spawn(|| wait_cancellable(|| cancel_a.load(Ordering::SeqCst), || a.cancel(), || a.wait()));
            let second = scope.spawn(|| wait_cancellable(|| cancel_b.load(Ordering::SeqCst), || b.cancel(), || b.wait()));
            std::thread::sleep(Duration::from_millis(100));
            cancel_a.store(true, Ordering::SeqCst);
            assert_eq!(first.join().unwrap(), Some("stopped"));
            // The other job was never asked to stop and still runs.
            std::thread::sleep(Duration::from_millis(150));
            assert!(!second.is_finished());
            assert_eq!(b.stops.load(Ordering::SeqCst), 0);
            b.release.store(true, Ordering::SeqCst);
            assert_eq!(second.join().unwrap(), Some("done"));
        });
        assert!(a.stops.load(Ordering::SeqCst) >= 1);
        assert_eq!(b.stops.load(Ordering::SeqCst), 0);
    }

    /// The reason the standard host could not be made is what the commands say, until a host is set.
    #[test]
    fn a_failed_standard_host_keeps_its_reason() {
        let state = AiState::default();
        assert_eq!(state.host().err().as_deref(), Some("AI is not set up in this frontend"));
        state.fail_host("no home folder to keep AI settings in".to_owned());
        assert_eq!(state.host().err().as_deref(), Some("no home folder to keep AI settings in"));
        assert_eq!(state.status_reason_now(DEPTH_TOOL).as_deref(), Some("no home folder to keep AI settings in"));
        // A host that works replaces the failure.
        let host = AiHost::new(SharedStore::new("/nonexistent/x.json"), || Err("later".to_owned()));
        state.set_host(host);
        assert!(state.host().is_ok());
        assert_eq!(state.status_reason_now(DEPTH_TOOL).as_deref(), Some("later"));
    }

    /// A shared state sees the parent's host and status; one made from a state without a host has none.
    #[test]
    fn a_shared_state_uses_the_parents_host() {
        let parent = AiState::default();
        assert!(parent.shared().host().is_err());
        parent.set_host(AiHost::new(SharedStore::new("/nonexistent/x.json"), || Err("parent host".to_owned())));
        let child = parent.shared();
        assert!(child.host().is_ok());
        assert_eq!(child.status_reason_now(DEPTH_TOOL).as_deref(), Some("parent host"));
    }

    /// A first `known_status_reason` while the background read runs waits for that read: the
    /// client is made once and the answer is the one the background read stored.
    #[test]
    fn a_first_status_read_waits_for_the_read_in_flight() {
        let made = Arc::new(AtomicUsize::new(0));
        let counter = made.clone();
        let state = AiState::default();
        state.set_host(AiHost::new(SharedStore::new("/nonexistent/x.json"), move || {
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            std::thread::sleep(Duration::from_millis(200));
            // A failure is not kept, so a second read would make a second client and say "read 2".
            Err(format!("read {n}"))
        }));
        // Wait until the background read is inside the slow client maker.
        while made.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
        // The answer is the background read's (not a Checking answer, not a second read).
        assert_eq!(state.known_status_reason(DEPTH_TOOL).as_deref(), Some("read 1"));
        assert_eq!(state.known_status_reason(DEPTH_TOOL).as_deref(), Some("read 1"));
        assert_eq!(made.load(Ordering::SeqCst), 1, "the status was read more than once");
    }

    /// Each tool has its own status: a stand-in for one leaves the other asking the layer, and the
    /// words say which tool is being checked.
    #[test]
    fn statuses_are_per_tool() {
        let mut state = AiState::default();
        state.set_host(AiHost::new(SharedStore::new("/nonexistent/x.json"), || Err("no client".to_owned())));
        state.segmenter = Some(Arc::new(|_, _, _, _| Err("unused".to_owned())));
        assert!(state.stands_in(SELECT_SUBJECT_TOOL) && !state.stands_in(DEPTH_TOOL));
        assert_eq!(state.status_reason_now(SELECT_SUBJECT_TOOL), None);
        assert_eq!(state.status_reason_now(DEPTH_TOOL).as_deref(), Some("no client"));
        let fresh = AiState::default();
        assert_eq!(fresh.status_reason(SELECT_SUBJECT_TOOL).as_deref(), Some("Checking Select Subject\u{2026}"));
        assert_eq!(fresh.status_reason(DEPTH_TOOL).as_deref(), Some("Checking Estimate Depth\u{2026}"));
    }

    #[test]
    fn a_panicking_wait_is_none() {
        let done: Option<u8> = wait_cancellable(|| false, || true, || -> u8 { std::panic::panic_any("boom") });
        assert_eq!(done, None);
    }
}
