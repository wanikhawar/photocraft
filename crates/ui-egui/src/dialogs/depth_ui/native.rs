//! The native Estimate Depth flow: estimate at once when the tool is ready, else walk the person
//! through the shared setup. See the parent module.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use craft_ai_client::{
    Adopted, CheckedNative, FolderDesk, MissingModel, ModelDownloader, NativeSetupPlan, SetupLink, SetupProgress, SetupSession, SetupTicket, ToolDesk,
};
use craft_ai_core::AiError;
use craft_ai_egui::{NativeSetupAction, PopupAction, missing_model_popup, native_model_setup};
use craft_ai_features::{Feature, builtin};
use photocraft_doc::DocId;
use photocraft_engine::depth_cmds::{AiHost, TOOL};
use serde_json::{Value, json};

use crate::PhotocraftApp;

/// What the setup window is doing, as `ui.inspect` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Reading what the tool needs.
    Loading,
    /// Showing the missing-model popup.
    Missing,
    /// Building the setup plan.
    Planning,
    /// Showing the setup screen (the person reviews, then downloads or checks files).
    Setup,
    /// The setup could not continue; the message is shown.
    Failed,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Loading => "loading",
            Phase::Missing => "missing",
            Phase::Planning => "planning",
            Phase::Setup => "setup",
            Phase::Failed => "failed",
        }
    }
}

/// A folder the person picked, and what it is for.
enum Folder {
    /// Nothing changes; the plan is just read again.
    Keep,
    /// The folder the suite suggests becomes the download folder.
    Suggested,
    /// The download folder.
    Destination(PathBuf),
    /// A folder that already holds model files, linked for discovery.
    Existing(PathBuf),
}

/// An answer from a worker thread.
enum Msg {
    Popup(Result<Option<MissingModel>, String>),
    Plan(Result<NativeSetupPlan, String>),
    Progress(SetupTicket, usize, SetupProgress),
    Checked(SetupTicket, Result<CheckedNative, AiError>),
    Chosen(Result<(), String>),
    Adopted(Result<Adopted, String>),
    /// A worker ended without an answer.
    Failed(String),
}

enum Stage {
    Loading,
    Missing(Box<MissingModel>),
    Planning,
    Setup(Box<SetupSession>),
    Failed,
}

/// The setup window. Bound to the document that started the estimate.
struct Window {
    doc: DocId,
    host: AiHost,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    stage: Stage,
    /// A worker is running that the window waits for (not the download, which the session tracks).
    working: bool,
    /// The first frame starts the first worker (it needs the egui context).
    started: bool,
    /// The provider of the setup option the person took (`native:<tool>:<provider>`).
    provider: Option<String>,
    error: Option<String>,
}

impl Window {
    fn phase(&self) -> Phase {
        match self.stage {
            Stage::Loading => Phase::Loading,
            Stage::Missing(_) => Phase::Missing,
            Stage::Planning => Phase::Planning,
            Stage::Setup(_) => Phase::Setup,
            Stage::Failed => Phase::Failed,
        }
    }
}

/// What the app keeps of Estimate Depth's setup. Not serde: it holds workers and shared-layer
/// state; `ui.inspect` shows [`inspect`].
#[derive(Default)]
pub struct DepthAi {
    window: Option<Window>,
    /// A folder the person picked, waiting for the next frame to start its plan worker (the file
    /// dialog's answer arrives without an egui context).
    pending: Option<Folder>,
}

fn message(e: AiError) -> String {
    e.message
}

fn feature() -> Result<Feature, String> {
    builtin().into_iter().find(|f| f.id == TOOL).ok_or_else(|| "Estimate Depth is not part of the AI tools yet".to_owned())
}

/// Run `work` on a worker thread and send its message back, waking the window. A worker that
/// panics or cannot start answers [`Msg::Failed`].
fn spawn(ctx: &egui::Context, tx: &Sender<Msg>, work: impl FnOnce() -> Msg + Send + 'static) -> Result<(), String> {
    let (ctx, tx) = (ctx.clone(), tx.clone());
    std::thread::Builder::new()
        .name("photocraft-depth-setup".into())
        .spawn(move || {
            let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(|_| Msg::Failed("Setting up Estimate Depth stopped unexpectedly".into()));
            // Nothing is waiting any more when the window was closed meanwhile.
            let _ = tx.send(answer);
            ctx.request_repaint();
        })
        .map(|_| ())
        .map_err(|e| format!("could not start the setup: {e}"))
}

fn load_popup(host: &AiHost) -> Result<Option<MissingModel>, String> {
    let desk = ToolDesk::new(host.client()?, host.shared().clone(), None);
    let role = feature()?.role().ok_or("the tool id is not valid")?;
    desk.missing_model(&role, &builtin()).map_err(message)
}

fn prepare(host: &AiHost, provider: &str, folder: Folder) -> Result<NativeSetupPlan, String> {
    let client = host.client()?;
    let desk = FolderDesk::new(host.shared().clone());
    match folder {
        Folder::Keep => {}
        Folder::Suggested => {
            desk.ensure_default().map_err(message)?;
        }
        Folder::Destination(path) => {
            let added = desk.add_new(&path, None).map_err(message)?;
            desk.set_default(&added.folder.id).map_err(message)?;
        }
        Folder::Existing(path) => {
            desk.add(&path, None).map_err(message)?;
        }
    }
    desk.prepare_native_setup(&feature()?, provider, &client).map_err(message)
}

/// The provider named by a setup option id `native:<tool>:<provider>`.
fn native_provider(option: &str) -> Option<String> {
    let mut parts = option.splitn(3, ':');
    (parts.next() == Some("native") && parts.next() == Some(TOOL)).then(|| parts.next().map(str::to_owned)).flatten()
}

/// The document the app shows, if any.
fn active_doc(app: &PhotocraftApp) -> Option<DocId> {
    app.session.active().map(|d| d.doc.id)
}

/// Why the Estimate Depth button is off, for its tooltip, or `None` while it can be used.
pub fn unavailable_reason(app: &PhotocraftApp) -> Option<String> {
    app.session.depth.ai.status_reason()
}

/// A small summary for `ui.inspect`: `null` while no setup window is open.
pub fn inspect(app: &PhotocraftApp) -> Value {
    match &app.depth_ai.window {
        None => Value::Null,
        Some(w) => json!({"phase": w.phase().name(), "busy": w.working || matches!(&w.stage, Stage::Setup(s) if s.busy()), "error": w.error}),
    }
}

/// Run `image.estimateDepth` into the brush's depth channel, as a background job when jobs are on.
fn estimate(app: &mut PhotocraftApp) {
    let surface = &app.session.tools.brush.surface;
    let (channel, quality) = (surface.depth_channel.clone(), if surface.depth_high_quality { "high" } else { "fast" });
    if let Err(e) = app.run("image.estimateDepth", json!({"channel": channel, "quality": quality})) {
        match app.depth_ai.window.as_mut() {
            Some(w) => {
                w.error = Some(e);
                w.stage = Stage::Failed;
            }
            None => {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
    }
}

/// The Estimate Depth… button: estimate at once when the tool is ready, else open the setup window.
pub fn start(app: &mut PhotocraftApp) {
    let Some(doc) = active_doc(app) else {
        app.ui.status = tl!("No document open").into();
        app.ui.status_error = true;
        return;
    };
    if app.session.depth.ai.status_reason().is_none() {
        estimate(app);
        return;
    }
    let host = match app.session.depth.ai.host() {
        Ok(h) => h,
        Err(e) => {
            app.ui.status = e;
            app.ui.status_error = true;
            return;
        }
    };
    let (tx, rx) = mpsc::channel();
    app.depth_ai.window = Some(Window { doc, host, tx, rx, stage: Stage::Loading, working: false, started: false, provider: None, error: None });
}

/// Close the window and stop its download.
pub fn cancel(app: &mut PhotocraftApp) {
    if let Some(mut w) = app.depth_ai.window.take()
        && let Stage::Setup(session) = &mut w.stage
    {
        session.cancel();
    }
}

/// Hand a worker's answer to the window. Returns true when the estimate should run now.
fn apply(app: &mut PhotocraftApp, msg: Msg) -> bool {
    let current = active_doc(app);
    let Some(w) = app.depth_ai.window.as_mut() else { return false };
    match msg {
        Msg::Failed(e) => {
            w.working = false;
            w.error = Some(e);
            if !matches!(w.stage, Stage::Setup(_)) {
                w.stage = Stage::Failed;
            }
        }
        Msg::Popup(r) => {
            w.working = false;
            match r {
                // The tool became ready meanwhile (another app set it up).
                Ok(None) => {
                    app.session.depth.ai.forget_status();
                    return finish(app, current);
                }
                Ok(Some(p)) => w.stage = Stage::Missing(Box::new(p)),
                Err(e) => {
                    w.error = Some(e);
                    w.stage = Stage::Failed;
                }
            }
        }
        Msg::Plan(r) => {
            w.working = false;
            match (r, &mut w.stage) {
                (Ok(plan), Stage::Setup(session)) => {
                    // A plan for the same target only refreshes what is shown.
                    session.review(plan);
                    w.error = None;
                }
                (Ok(plan), stage) => {
                    *stage = Stage::Setup(Box::new(SetupSession::new(plan)));
                    w.error = None;
                }
                (Err(e), stage) => {
                    w.error = Some(e);
                    if !matches!(stage, Stage::Setup(_)) {
                        *stage = Stage::Failed;
                    }
                }
            }
        }
        Msg::Progress(ticket, file, progress) => {
            if let Stage::Setup(session) = &mut w.stage {
                session.report_progress(ticket, file, progress);
            }
        }
        Msg::Checked(ticket, r) => {
            if let Stage::Setup(session) = &mut w.stage {
                session.complete(ticket, r);
            }
        }
        Msg::Chosen(r) => {
            w.working = false;
            match r {
                Ok(()) => {
                    app.session.depth.ai.forget_status();
                    return finish(app, current);
                }
                Err(e) => w.error = Some(e),
            }
        }
        Msg::Adopted(r) => {
            w.working = false;
            match r {
                Ok(adopted) => {
                    app.session.depth.ai.forget_status();
                    if let Some((connection, model)) = adopted.kept.filter(|_| !adopted.bound) {
                        w.error = Some(format!(
                            "Another model ({} / {}) is already chosen for Estimate Depth, so the new files were not switched on. Choose it in Set up AI in the ArtCraft Launcher.",
                            connection.as_str(),
                            model.as_str()
                        ));
                        return false;
                    }
                    return finish(app, current);
                }
                Err(e) => w.error = Some(e),
            }
        }
    }
    false
}

/// Setup is done: estimate when the person is still on the document that asked, else say so and
/// leave nothing running.
fn finish(app: &mut PhotocraftApp, current: Option<DocId>) -> bool {
    let Some(w) = app.depth_ai.window.as_mut() else { return false };
    if app.session.depth.ai.status_reason().is_some() {
        w.error = Some(app.session.depth.ai.status_reason().unwrap_or_default());
        w.stage = Stage::Failed;
        return false;
    }
    if current != Some(w.doc) {
        w.error = Some(
            tl!("Estimate Depth is set up. The document it was started for is no longer the active one: switch back to it and choose Estimate Depth again.")
                .into(),
        );
        w.stage = Stage::Failed;
        return false;
    }
    app.depth_ai.window = None;
    true
}

/// Ask for a folder, then rebuild the plan with it. `existing` is true for a folder that already
/// holds model files (linked for discovery), false for the folder downloads go to.
fn pick_folder(app: &mut PhotocraftApp, existing: bool) {
    let asked = app.ask_folder(move |app, path| {
        if app.depth_ai.window.is_some() {
            let path = PathBuf::from(path);
            app.depth_ai.pending = Some(if existing { Folder::Existing(path) } else { Folder::Destination(path) });
        }
        Ok(Value::Null)
    });
    if let (Err(e), Some(w)) = (asked, app.depth_ai.window.as_mut()) {
        w.error = Some(e);
    }
}

/// Draw the window and step it: take workers' answers, draw the current screen, act on it.
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.depth_ai.window.is_none() {
        return;
    }
    // Start the first worker.
    if let Some(w) = app.depth_ai.window.as_mut()
        && !w.started
    {
        w.started = true;
        w.working = true;
        let host = w.host.clone();
        if let Err(e) = spawn(ctx, &w.tx, move || Msg::Popup(load_popup(&host))) {
            w.working = false;
            w.error = Some(e);
            w.stage = Stage::Failed;
        }
    }
    // A folder picked on an earlier frame starts its plan worker now.
    if let Some(folder) = app.depth_ai.pending.take()
        && let Some(w) = app.depth_ai.window.as_mut()
        && let Some(provider) = w.provider.clone()
    {
        let host = w.host.clone();
        w.working = true;
        w.error = None;
        if let Err(e) = spawn(ctx, &w.tx, move || Msg::Plan(prepare(&host, &provider, folder))) {
            w.working = false;
            w.error = Some(e);
        }
    }
    // Take answers.
    let mut run_estimate = false;
    while let Some(w) = app.depth_ai.window.as_ref() {
        match w.rx.try_recv() {
            Ok(msg) => {
                if apply(app, msg) {
                    run_estimate = true;
                    break;
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    if run_estimate {
        estimate(app);
        return;
    }
    let Some(w) = app.depth_ai.window.as_mut() else { return };
    if w.working || matches!(&w.stage, Stage::Setup(s) if s.busy()) {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    /// What the person asked for this frame.
    enum Act {
        None,
        Close,
        Choose(craft_ai_core::ConnectionId, craft_ai_core::ModelId),
        SetUp(SetupLink),
        Setup(Box<NativeSetupAction>),
        Back,
    }
    let mut act = Act::None;
    let mut open = true;
    egui::Window::new(tl!("Estimate Depth")).open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).show(
        ctx,
        |ui| {
            ui.set_max_width(420.0);
            match &mut w.stage {
                Stage::Loading | Stage::Planning => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tl!("Working…"));
                    });
                }
                Stage::Missing(popup) => {
                    ui.add_enabled_ui(!w.working, |ui| match missing_model_popup(ui, popup) {
                        Some(PopupAction::Choose { connection, model }) => act = Act::Choose(connection, model),
                        Some(PopupAction::SetUp(link)) => act = Act::SetUp(link),
                        Some(PopupAction::NotNow) => act = Act::Close,
                        // The install offer is not drawn here.
                        Some(PopupAction::InstallComfyUi(_)) | None => {}
                    });
                }
                Stage::Setup(session) => {
                    ui.add_enabled_ui(!w.working, |ui| {
                        if let Some(action) = native_model_setup(ui, session) {
                            act = match action {
                                NativeSetupAction::Back => Act::Back,
                                other => Act::Setup(Box::new(other)),
                            };
                        }
                    });
                }
                Stage::Failed => {
                    if ui.button(tl!("Close")).clicked() {
                        act = Act::Close;
                    }
                }
            }
            if let Some(e) = &w.error {
                ui.add_space(6.0);
                ui.colored_label(crate::theme::Tokens::get(ui.ctx()).danger, e);
            }
        },
    );
    if !open {
        act = Act::Close;
    }
    match act {
        Act::None => {}
        Act::Close => cancel(app),
        Act::Back => {
            if let Some(w) = app.depth_ai.window.as_mut() {
                if let Stage::Setup(session) = &mut w.stage {
                    session.cancel();
                }
                w.stage = Stage::Loading;
                w.started = false;
                w.error = None;
            }
        }
        Act::Choose(connection, model) => {
            if let Some(w) = app.depth_ai.window.as_mut() {
                w.working = true;
                w.error = None;
                let host = w.host.clone();
                let started = spawn(ctx, &w.tx, move || {
                    let run = || -> Result<(), String> {
                        let desk = ToolDesk::new(host.client()?, host.shared().clone(), None);
                        let role = feature()?.role().ok_or("the tool id is not valid")?;
                        desk.choose(&role, &connection, &model).map_err(message)
                    };
                    Msg::Chosen(run())
                });
                if let Err(e) = started {
                    w.working = false;
                    w.error = Some(e);
                }
            }
        }
        Act::SetUp(link) => {
            let provider = match &link {
                SetupLink::New { option } => native_provider(option),
                SetupLink::Fix { .. } => None,
            };
            if let Some(w) = app.depth_ai.window.as_mut() {
                match provider {
                    Some(provider) => {
                        w.provider = Some(provider.clone());
                        w.stage = Stage::Planning;
                        w.working = true;
                        w.error = None;
                        let host = w.host.clone();
                        if let Err(e) = spawn(ctx, &w.tx, move || Msg::Plan(prepare(&host, &provider, Folder::Keep))) {
                            w.working = false;
                            w.error = Some(e);
                            w.stage = Stage::Failed;
                        }
                    }
                    None => w.error = Some(tl!("Open Set up AI in the ArtCraft Launcher to set this up.").into()),
                }
            }
        }
        Act::Setup(action) => setup_action(app, ctx, *action),
    }
}

/// Carry out what the setup screen asked for.
fn setup_action(app: &mut PhotocraftApp, ctx: &egui::Context, action: NativeSetupAction) {
    match action {
        NativeSetupAction::Back | NativeSetupAction::Cancel(_) => {}
        NativeSetupAction::FindFiles => pick_folder(app, true),
        NativeSetupAction::ChangeFolder => pick_folder(app, false),
        NativeSetupAction::UseSuggestedFolder => {
            let Some(w) = app.depth_ai.window.as_mut() else { return };
            let Some(provider) = w.provider.clone() else { return };
            w.working = true;
            w.error = None;
            let host = w.host.clone();
            if let Err(e) = spawn(ctx, &w.tx, move || Msg::Plan(prepare(&host, &provider, Folder::Suggested))) {
                w.working = false;
                w.error = Some(e);
            }
        }
        NativeSetupAction::Work(request) => {
            let Some(w) = app.depth_ai.window.as_mut() else { return };
            w.error = None;
            let (tx, repaint) = (w.tx.clone(), ctx.clone());
            let ticket = request.ticket();
            let started = spawn(ctx, &w.tx, move || {
                let downloader = ModelDownloader::new(Default::default());
                let progress_tx = tx.clone();
                let result = request.run(&downloader, None, &mut |file, progress| {
                    // The window may be gone; the download then ends at its cancel flag.
                    let _ = progress_tx.send(Msg::Progress(ticket, file, progress));
                    repaint.request_repaint();
                });
                Msg::Checked(ticket, result)
            });
            if let Err(e) = started {
                w.error = Some(e);
                if let Stage::Setup(session) = &mut w.stage {
                    session.cancel();
                }
            }
        }
        NativeSetupAction::Use(checked) => {
            let Some(w) = app.depth_ai.window.as_mut() else { return };
            w.working = true;
            w.error = None;
            let host = w.host.clone();
            let started = spawn(ctx, &w.tx, move || {
                let run = || -> Result<Adopted, String> {
                    let desk = ToolDesk::new(host.client()?, host.shared().clone(), None);
                    desk.adopt(&checked).map_err(message)
                };
                Msg::Adopted(run())
            });
            if let Err(e) = started {
                w.working = false;
                w.error = Some(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// A host over an empty temporary AI store: nothing is set up, and no test reads a person's settings.
    fn empty_host(dir: &std::path::Path) -> AiHost {
        let store = craft_ai_native::shared::SharedStore::new(dir.join("connections.json"));
        let shared = store.clone();
        AiHost::new(store, move || {
            let app = craft_ai_core::AppId::new("photocraft").map_err(|e| e.to_string())?;
            Ok(craft_ai_client::AiClient::new(app, shared.clone(), Arc::new(craft_ai_native::secrets::MemoryStore::new())))
        })
    }

    fn app(dir: &std::path::Path) -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 32, "height": 24})).unwrap();
        app.session.depth.ai.set_host(empty_host(dir));
        app
    }

    #[test]
    fn setup_finishing_on_another_document_does_not_estimate() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        let started_on = active_doc(&app).unwrap();
        start(&mut app);
        assert_eq!(app.depth_ai.window.as_ref().map(|w| w.doc), Some(started_on));
        // The person switches to another document while the setup runs.
        app.run("file.new", json!({"width": 16, "height": 16})).unwrap();
        assert_ne!(active_doc(&app), Some(started_on));
        // The tool is ready by the time the setup ends.
        app.session.depth.ai.estimator = Some(Arc::new(|_, _, _, _| Err("not run".into())));
        assert!(!apply(&mut app, Msg::Chosen(Ok(()))));
        assert_eq!(inspect(&app)["phase"], "failed");
        let error = inspect(&app)["error"].as_str().unwrap_or_default().to_owned();
        assert!(error.contains("no longer the active one"), "{error}");
        assert!(app.session.active().unwrap().doc.channels.is_empty());
        // On the document that asked, setup finishing runs the estimate and closes the window.
        let current = active_doc(&app).unwrap();
        if let Some(w) = app.depth_ai.window.as_mut() {
            w.doc = current;
            w.stage = Stage::Loading;
        }
        assert!(apply(&mut app, Msg::Chosen(Ok(()))));
        assert!(app.depth_ai.window.is_none());
    }

    #[test]
    fn setup_that_leaves_the_tool_unavailable_reports_why() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        start(&mut app);
        assert!(!apply(&mut app, Msg::Chosen(Ok(()))));
        assert_eq!(inspect(&app)["phase"], "failed");
        assert!(inspect(&app)["error"].is_string());
    }

    #[test]
    fn a_failed_worker_is_shown_and_stops_the_wait() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        start(&mut app);
        if let Some(w) = app.depth_ai.window.as_mut() {
            w.working = true;
        }
        assert!(!apply(&mut app, Msg::Failed("boom".into())));
        assert_eq!(inspect(&app), json!({"phase": "failed", "busy": false, "error": "boom"}));
    }

    #[test]
    fn the_native_provider_is_read_from_the_setup_option() {
        assert_eq!(native_provider("native:estimate-depth:candle").as_deref(), Some("candle"));
        assert_eq!(native_provider("native:select-subject:candle"), None);
        assert_eq!(native_provider("local:estimate-depth:x"), None);
        assert_eq!(native_provider("service:openai"), None);
        assert_eq!(native_provider("native:estimate-depth"), None);
    }
}
