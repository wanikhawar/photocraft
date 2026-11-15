//! The native AI tool setup flow: run the follow-up at once when the tool is ready, else walk the
//! person through the shared setup. See the parent module.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use craft_ai_client::{
    Adopted, CheckedNative, FolderDesk, MissingModel, ModelDownloader, NativeSetupPlan, SetupLink, SetupProgress, SetupSession, SetupTicket, ToolDesk,
};
use craft_ai_core::AiError;
use craft_ai_egui::{NativeSetupAction, PopupAction, missing_model_popup, native_model_setup};
use craft_ai_features::{Feature, builtin};
use photocraft_doc::DocId;
use photocraft_engine::ai_layer::AiHost;
use photocraft_engine::ai_layer::{DEPTH_TOOL, SELECT_SUBJECT_TOOL};
use serde_json::{Value, json};

use super::After;
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
pub(crate) enum Folder {
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
pub(crate) enum Msg {
    Popup(Result<Option<MissingModel>, String>),
    Plan(Result<NativeSetupPlan, String>),
    Progress(SetupTicket, usize, SetupProgress),
    Checked(SetupTicket, Result<CheckedNative, AiError>),
    Chosen(Result<(), String>),
    Adopted(Result<Adopted, String>),
    /// A worker ended without an answer.
    Failed(String),
}

pub(crate) enum Stage {
    Loading,
    Missing(Box<MissingModel>),
    Planning,
    Setup(Box<SetupSession>),
    Failed,
}

/// The setup window. Bound to the document that started it, when what follows needs one.
pub(crate) struct Window {
    /// Which setup this is. A new one is made by [`start`] and by Back, so an answer (a picked
    /// folder) meant for an earlier setup is told apart from this one's.
    pub(crate) id: u64,
    /// The tool being set up (`DEPTH_TOOL`, `SELECT_SUBJECT_TOOL`).
    pub(crate) tool: &'static str,
    /// The document that asked, when `after` needs it to still be the active one.
    pub(crate) doc: Option<DocId>,
    /// What runs when setup ends (fixed when the person asked).
    pub(crate) after: After,
    pub(crate) host: AiHost,
    pub(crate) tx: Sender<Msg>,
    pub(crate) rx: Receiver<Msg>,
    pub(crate) stage: Stage,
    /// A worker is running that the window waits for (not the download, which the session tracks).
    pub(crate) working: bool,
    /// The first frame starts the first worker (it needs the egui context).
    pub(crate) started: bool,
    /// The provider of the setup option the person took (`native:<tool>:<provider>`).
    pub(crate) provider: Option<String>,
    pub(crate) error: Option<String>,
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

/// What the app keeps of an AI tool's setup. Not serde: it holds workers and shared-layer
/// state; `ui.inspect` shows [`inspect`].
#[derive(Default)]
pub struct AiSetup {
    pub(crate) window: Option<Window>,
    /// The id the next setup gets.
    pub(crate) next_id: u64,
    /// A folder the person picked, waiting for the next frame to start its plan worker (the file
    /// dialog's answer arrives without an egui context).
    pub(crate) pending: Option<Folder>,
}

fn message(e: AiError) -> String {
    e.message
}

fn feature(tool: &str) -> Result<Feature, String> {
    builtin().into_iter().find(|f| f.id == tool).ok_or_else(|| format!("{} is not part of the AI tools yet", label(tool)))
}

/// What the tool is called in a sentence.
fn label(tool: &str) -> &'static str {
    match tool {
        DEPTH_TOOL => "Estimate Depth",
        SELECT_SUBJECT_TOOL => "AI object finding",
        _ => "this tool",
    }
}

/// The window's title.
fn title(tool: &str) -> &'static str {
    match tool {
        DEPTH_TOOL => tl!("Estimate Depth"),
        SELECT_SUBJECT_TOOL => tl!("Set Up AI Object Finding"),
        _ => tl!("Set Up AI"),
    }
}

/// Run `work` on a worker thread and send its message back, waking the window. A worker that
/// panics or cannot start answers [`Msg::Failed`].
fn spawn(ctx: &egui::Context, tx: &Sender<Msg>, tool: &str, work: impl FnOnce() -> Msg + Send + 'static) -> Result<(), String> {
    let (ctx, tx) = (ctx.clone(), tx.clone());
    let stopped = format!("Setting up {} stopped unexpectedly", label(tool));
    std::thread::Builder::new()
        .name("photocraft-ai-setup".into())
        .spawn(move || {
            let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or(Msg::Failed(stopped));
            // Nothing is waiting any more when the window was closed meanwhile.
            let _ = tx.send(answer);
            ctx.request_repaint();
        })
        .map(|_| ())
        .map_err(|e| format!("could not start the setup: {e}"))
}

fn load_popup(host: &AiHost, tool: &str) -> Result<Option<MissingModel>, String> {
    let desk = ToolDesk::new(host.client()?, host.shared().clone(), None);
    let role = feature(tool)?.role().ok_or("the tool id is not valid")?;
    desk.missing_model(&role, &builtin()).map_err(message)
}

pub(crate) fn prepare(host: &AiHost, tool: &str, provider: &str, folder: Folder) -> Result<NativeSetupPlan, String> {
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
    desk.prepare_native_setup(&feature(tool)?, provider, &client).map_err(message)
}

/// The provider named by a setup option id `native:<tool>:<provider>` for `tool`.
pub(crate) fn native_provider(option: &str, tool: &str) -> Option<String> {
    let mut parts = option.splitn(3, ':');
    (parts.next() == Some("native") && parts.next() == Some(tool)).then(|| parts.next().map(str::to_owned)).flatten()
}

/// The document the app shows, if any.
pub(crate) fn active_doc(app: &PhotocraftApp) -> Option<DocId> {
    app.session.active().map(|d| d.doc.id)
}

/// A small summary for `ui.inspect`: `null` while no setup window is open.
pub fn inspect(app: &PhotocraftApp) -> Value {
    match &app.ai_setup.window {
        None => Value::Null,
        Some(w) => json!({"phase": w.phase().name(), "busy": w.working || matches!(&w.stage, Stage::Setup(s) if s.busy()), "error": w.error}),
    }
}

/// Run what follows a finished setup: Estimate Depth runs `image.estimateDepth` as its `Ask` says (as
/// a background job when jobs are on); Stay on Object has nothing to run.
fn run_after(app: &mut PhotocraftApp, after: &After) {
    match after {
        After::Nothing => {}
        After::EstimateDepth(ask) => {
            if let Err(e) = app.run("image.estimateDepth", json!({"channel": ask.channel, "quality": ask.quality})) {
                match app.ai_setup.window.as_mut() {
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
    }
}

/// A tool's button: run `after` at once when the tool is ready, else open the setup window for it.
/// `doc` is the document `after` needs to still be the active one, if any.
pub fn start(app: &mut PhotocraftApp, tool: &'static str, after: After, doc: Option<DocId>) {
    // A window already open keeps its setup (its download, its choices): replacing it would drop
    // a running download without stopping it. A failed one has nothing running and is replaced.
    if let Some(w) = &app.ai_setup.window {
        if !matches!(w.stage, Stage::Failed) {
            return;
        }
        cancel(app);
    }
    if app.session.ai.status_reason_now(tool).is_none() {
        run_after(app, &after);
        return;
    }
    let host = match app.session.ai.host() {
        Ok(h) => h,
        Err(e) => {
            app.ui.status = e;
            app.ui.status_error = true;
            return;
        }
    };
    let (tx, rx) = mpsc::channel();
    let id = new_setup_id(app);
    app.ai_setup.window =
        Some(Window { id, tool, doc, after, host, tx, rx, stage: Stage::Loading, working: false, started: false, provider: None, error: None });
}

/// A fresh setup identity.
fn new_setup_id(app: &mut PhotocraftApp) -> u64 {
    app.ai_setup.next_id = app.ai_setup.next_id.wrapping_add(1);
    app.ai_setup.next_id
}

/// Close the window when the document it was started for is no longer open: its setup could only
/// end in a follow-up for a document that is gone. A setup with no document stays.
pub fn cancel_stale(app: &mut PhotocraftApp) {
    let gone = app.ai_setup.window.as_ref().is_some_and(|w| w.doc.is_some_and(|doc| !app.session.documents().iter().any(|d| d.doc.id == doc)));
    if gone {
        cancel(app);
    }
}

/// Close the window and stop its download. A folder picked for it and not yet used is dropped.
pub fn cancel(app: &mut PhotocraftApp) {
    app.ai_setup.pending = None;
    if let Some(mut w) = app.ai_setup.window.take()
        && let Stage::Setup(session) = &mut w.stage
    {
        session.cancel();
    }
}

/// Hand a worker's answer to the window. Returns what to run when setup ended and it should run now.
pub(crate) fn apply(app: &mut PhotocraftApp, msg: Msg) -> Option<After> {
    let current = active_doc(app);
    let w = app.ai_setup.window.as_mut()?;
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
                    app.session.ai.forget_status();
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
                    app.session.ai.forget_status();
                    return finish(app, current);
                }
                Err(e) => w.error = Some(e),
            }
        }
        Msg::Adopted(r) => {
            w.working = false;
            match r {
                Ok(adopted) => {
                    app.session.ai.forget_status();
                    if let Some((connection, model)) = adopted.kept.filter(|_| !adopted.bound) {
                        w.error = Some(format!(
                            "Another model ({} / {}) is already chosen for {}, so the new files were not switched on. Choose it in Set up AI in the ArtCraft Launcher.",
                            connection.as_str(),
                            model.as_str(),
                            label(w.tool)
                        ));
                        return None;
                    }
                    return finish(app, current);
                }
                Err(e) => w.error = Some(e),
            }
        }
    }
    None
}

/// Setup is done: run what follows when the person is still on the document that asked (if it
/// needs one), else say so and leave nothing running.
fn finish(app: &mut PhotocraftApp, current: Option<DocId>) -> Option<After> {
    let w = app.ai_setup.window.as_mut()?;
    if let Some(reason) = app.session.ai.status_reason_now(w.tool) {
        w.error = Some(reason);
        w.stage = Stage::Failed;
        return None;
    }
    if w.doc.is_some() && current != w.doc {
        w.error = Some(
            tl!("Estimate Depth is set up. The document it was started for is no longer the active one: switch back to it and choose Estimate Depth again.")
                .into(),
        );
        w.stage = Stage::Failed;
        return None;
    }
    app.ai_setup.window.take().map(|w| w.after)
}

/// Ask for a folder, then rebuild the plan with it. `existing` is true for a folder that already
/// holds model files (linked for discovery), false for the folder downloads go to.
fn pick_folder(app: &mut PhotocraftApp, existing: bool) {
    // The file dialog answers later: only the setup that asked takes the answer.
    let Some(id) = app.ai_setup.window.as_ref().map(|w| w.id) else { return };
    let asked = app.ask_folder(move |app, path| {
        folder_answered(app, id, existing, path);
        Ok(Value::Null)
    });
    if let (Err(e), Some(w)) = (asked, app.ai_setup.window.as_mut()) {
        w.error = Some(e);
    }
}

/// A folder was picked for the setup `id`. It is kept only while that setup is the open one; after
/// Back or a close (and a new start) it belongs to nobody.
pub(crate) fn folder_answered(app: &mut PhotocraftApp, id: u64, existing: bool, path: String) {
    if app.ai_setup.window.as_ref().is_some_and(|w| w.id == id) {
        let path = PathBuf::from(path);
        app.ai_setup.pending = Some(if existing { Folder::Existing(path) } else { Folder::Destination(path) });
    }
}

/// Draw the window and step it: take workers' answers, draw the current screen, act on it.
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.ai_setup.window.is_none() {
        return;
    }
    // Start the first worker.
    if let Some(w) = app.ai_setup.window.as_mut()
        && !w.started
    {
        w.started = true;
        w.working = true;
        let (host, tool) = (w.host.clone(), w.tool);
        if let Err(e) = spawn(ctx, &w.tx, tool, move || Msg::Popup(load_popup(&host, tool))) {
            w.working = false;
            w.error = Some(e);
            w.stage = Stage::Failed;
        }
    }
    // A folder picked on an earlier frame starts its plan worker now.
    if let Some(folder) = app.ai_setup.pending.take()
        && let Some(w) = app.ai_setup.window.as_mut()
        && let Some(provider) = w.provider.clone()
    {
        let (host, tool) = (w.host.clone(), w.tool);
        w.working = true;
        w.error = None;
        if let Err(e) = spawn(ctx, &w.tx, tool, move || Msg::Plan(prepare(&host, tool, &provider, folder))) {
            w.working = false;
            w.error = Some(e);
        }
    }
    // Take answers.
    let mut run_follow_up = None;
    while let Some(w) = app.ai_setup.window.as_ref() {
        match w.rx.try_recv() {
            Ok(msg) => {
                if let Some(after) = apply(app, msg) {
                    run_follow_up = Some(after);
                    break;
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    if let Some(after) = run_follow_up {
        run_after(app, &after);
        return;
    }
    let Some(w) = app.ai_setup.window.as_mut() else { return };
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
    egui::Window::new(title(w.tool)).open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).show(
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
        Act::Back => go_back(app),
        Act::Choose(connection, model) => {
            if let Some(w) = app.ai_setup.window.as_mut() {
                w.working = true;
                w.error = None;
                let (host, tool) = (w.host.clone(), w.tool);
                let started = spawn(ctx, &w.tx, tool, move || {
                    let run = || -> Result<(), String> {
                        let desk = ToolDesk::new(host.client()?, host.shared().clone(), None);
                        let role = feature(tool)?.role().ok_or("the tool id is not valid")?;
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
                SetupLink::New { option } => app.ai_setup.window.as_ref().and_then(|w| native_provider(option, w.tool)),
                SetupLink::Fix { .. } => None,
            };
            if let Some(w) = app.ai_setup.window.as_mut() {
                match provider {
                    Some(provider) => {
                        w.provider = Some(provider.clone());
                        w.stage = Stage::Planning;
                        w.working = true;
                        w.error = None;
                        let (host, tool) = (w.host.clone(), w.tool);
                        if let Err(e) = spawn(ctx, &w.tx, tool, move || Msg::Plan(prepare(&host, tool, &provider, Folder::Keep))) {
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

/// Back from the setup screen to the first one: stops the download and starts reading again. The
/// answers channel is replaced, so a worker still running from before (a plan, a choice, an
/// adoption) sends into a channel nobody reads and cannot move this window to a stale screen.
pub(crate) fn go_back(app: &mut PhotocraftApp) {
    // It restarts the setup: a folder picked for the old one is not this one's.
    app.ai_setup.pending = None;
    let id = new_setup_id(app);
    if let Some(w) = app.ai_setup.window.as_mut() {
        w.id = id;
        if let Stage::Setup(session) = &mut w.stage {
            session.cancel();
        }
        let (tx, rx) = mpsc::channel();
        w.tx = tx;
        w.rx = rx;
        w.working = false;
        w.stage = Stage::Loading;
        w.started = false;
        w.error = None;
    }
}

/// Carry out what the setup screen asked for.
fn setup_action(app: &mut PhotocraftApp, ctx: &egui::Context, action: NativeSetupAction) {
    match action {
        NativeSetupAction::Back | NativeSetupAction::Cancel(_) => {}
        NativeSetupAction::FindFiles => pick_folder(app, true),
        NativeSetupAction::ChangeFolder => pick_folder(app, false),
        NativeSetupAction::UseSuggestedFolder => {
            let Some(w) = app.ai_setup.window.as_mut() else { return };
            let Some(provider) = w.provider.clone() else { return };
            w.working = true;
            w.error = None;
            let (host, tool) = (w.host.clone(), w.tool);
            if let Err(e) = spawn(ctx, &w.tx, tool, move || Msg::Plan(prepare(&host, tool, &provider, Folder::Suggested))) {
                w.working = false;
                w.error = Some(e);
            }
        }
        NativeSetupAction::Work(request) => {
            let Some(w) = app.ai_setup.window.as_mut() else { return };
            w.error = None;
            let (tx, repaint) = (w.tx.clone(), ctx.clone());
            let ticket = request.ticket();
            let started = spawn(ctx, &w.tx, w.tool, move || {
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
            let Some(w) = app.ai_setup.window.as_mut() else { return };
            w.working = true;
            w.error = None;
            let (host, tool) = (w.host.clone(), w.tool);
            let started = spawn(ctx, &w.tx, tool, move || {
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
