//! Estimate Depth through the shared setup window (see `dialogs::ai_setup`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;

use craft_ai_client::SetupSession;
use photocraft_engine::ai_layer::{AiHost, DEPTH_TOOL};
use serde_json::json;

use super::start;
use crate::PhotocraftApp;
use crate::dialogs::ai_setup::*;

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
    app.session.ai.set_host(empty_host(dir));
    app
}

#[test]
fn setup_finishing_on_another_document_does_not_estimate() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    let started_on = active_doc(&app).unwrap();
    start(&mut app);
    assert_eq!(app.ai_setup.window.as_ref().and_then(|w| w.doc), Some(started_on));
    // The person switches to another document while the setup runs.
    app.run("file.new", json!({"width": 16, "height": 16})).unwrap();
    assert_ne!(active_doc(&app), Some(started_on));
    // The tool is ready by the time the setup ends.
    app.session.ai.estimator = Some(Arc::new(|_, _, _, _| Err("not run".into())));
    assert!(apply(&mut app, Msg::Chosen(Ok(()))).is_none());
    assert_eq!(inspect(&app)["phase"], "failed");
    let error = inspect(&app)["error"].as_str().unwrap_or_default().to_owned();
    assert!(error.contains("no longer the active one"), "{error}");
    assert!(app.session.active().unwrap().doc.channels.is_empty());
    // On the document that asked, setup finishing runs the estimate and closes the window.
    let current = active_doc(&app).unwrap();
    if let Some(w) = app.ai_setup.window.as_mut() {
        w.doc = Some(current);
        w.stage = Stage::Loading;
    }
    assert!(apply(&mut app, Msg::Chosen(Ok(()))).is_some());
    assert!(app.ai_setup.window.is_none());
}

/// A second Estimate Depth while a download runs keeps the window and its download.
#[test]
fn a_second_start_keeps_the_running_download() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    start(&mut app);
    let host = app.ai_setup.window.as_ref().map(|w| w.host.clone()).unwrap();
    let plan = prepare(&host, DEPTH_TOOL, "candle", Folder::Destination(dir.path().join("models"))).expect("a setup plan");
    let mut session = SetupSession::new(plan);
    session.accept_licences(true);
    let _request = session.start().expect("the download starts");
    assert!(session.busy());
    if let Some(w) = app.ai_setup.window.as_mut() {
        w.stage = Stage::Setup(Box::new(session));
        w.provider = Some("candle".into());
    }
    let started_on = active_doc(&app);
    // Another document, and the button again.
    app.run("file.new", json!({"width": 16, "height": 16})).unwrap();
    start(&mut app);
    let w = app.ai_setup.window.as_ref().expect("the window is kept");
    assert_eq!(w.doc, started_on, "still bound to the document that asked");
    assert_eq!(w.provider.as_deref(), Some("candle"));
    assert!(matches!(&w.stage, Stage::Setup(s) if s.busy()), "the download was not dropped");
    // A failed window has nothing running and is replaced.
    if let Some(w) = app.ai_setup.window.as_mut() {
        w.stage = Stage::Failed;
    }
    start(&mut app);
    assert_eq!(inspect(&app)["phase"], "loading");
}

/// An answer from a worker started before Back cannot reach the window.
#[test]
fn an_answer_from_before_back_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    start(&mut app);
    let stale = app.ai_setup.window.as_ref().map(|w| w.tx.clone()).unwrap();
    if let Some(w) = app.ai_setup.window.as_mut() {
        w.stage = Stage::Planning;
        w.working = true;
    }
    go_back(&mut app);
    let w = app.ai_setup.window.as_ref().unwrap();
    assert!(!w.working);
    assert_eq!(inspect(&app)["phase"], "loading");
    // The stale worker's answer goes nowhere, and nothing is waiting in the window's channel.
    assert!(stale.send(Msg::Failed("stale".into())).is_err());
    assert!(matches!(app.ai_setup.window.as_ref().unwrap().rx.try_recv(), Err(TryRecvError::Empty)));
}

/// Closing the document the setup was started for closes the window (and stops its download).
#[test]
fn closing_the_document_closes_the_setup_window() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    app.run("file.new", json!({"width": 16, "height": 16})).unwrap();
    start(&mut app);
    let started_on = app.ai_setup.window.as_ref().and_then(|w| w.doc).unwrap();
    // The window's document is still open: nothing is cancelled.
    cancel_stale(&mut app);
    assert!(app.ai_setup.window.is_some());
    // Closed behind the app's back, so the explicit cleanup is what is tested.
    app.session.execute("file.close", json!({})).unwrap();
    assert!(!app.session.documents().iter().any(|d| d.doc.id == started_on));
    cancel_stale(&mut app);
    assert!(app.ai_setup.window.is_none());
}

/// A folder answered after Back, or after the window was closed and another one opened, is not
/// taken by the new setup.
#[test]
fn a_folder_answer_for_an_earlier_setup_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    start(&mut app);
    let asked = app.ai_setup.window.as_ref().map(|w| w.id).unwrap();
    // The answer for the setup that asked is kept.
    folder_answered(&mut app, asked, false, "/models".into());
    assert!(matches!(&app.ai_setup.pending, Some(Folder::Destination(p)) if p == &PathBuf::from("/models")));
    app.ai_setup.pending = None;
    // After Back, the setup is a new one.
    go_back(&mut app);
    assert_ne!(app.ai_setup.window.as_ref().map(|w| w.id), Some(asked));
    folder_answered(&mut app, asked, true, "/models".into());
    assert!(app.ai_setup.pending.is_none());
    // After a close and a new start too, and a pick waiting at the close is dropped.
    let current = app.ai_setup.window.as_ref().map(|w| w.id).unwrap();
    folder_answered(&mut app, current, true, "/found".into());
    assert!(matches!(&app.ai_setup.pending, Some(Folder::Existing(_))));
    cancel(&mut app);
    assert!(app.ai_setup.pending.is_none());
    start(&mut app);
    assert_ne!(app.ai_setup.window.as_ref().map(|w| w.id), Some(current));
    folder_answered(&mut app, current, false, "/models".into());
    assert!(app.ai_setup.pending.is_none());
}

#[test]
fn setup_that_leaves_the_tool_unavailable_reports_why() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    start(&mut app);
    assert!(apply(&mut app, Msg::Chosen(Ok(()))).is_none());
    assert_eq!(inspect(&app)["phase"], "failed");
    assert!(inspect(&app)["error"].is_string());
}

#[test]
fn a_failed_worker_is_shown_and_stops_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    start(&mut app);
    if let Some(w) = app.ai_setup.window.as_mut() {
        w.working = true;
    }
    assert!(apply(&mut app, Msg::Failed("boom".into())).is_none());
    assert_eq!(inspect(&app), json!({"phase": "failed", "busy": false, "error": "boom"}));
}

/// The model files are in place, by download or by files found on disk: both end in `adopt`.
fn adopted() -> Msg {
    let connection = craft_ai_core::ConnectionId::new("native-estimate-depth-candle").unwrap();
    let model = craft_ai_core::ModelId::new("depth-anything-v2-small").unwrap();
    Msg::Adopted(Ok(craft_ai_client::Adopted { connection, model, bound: true, kept: None }))
}

/// Download completion and local-file completion on another document: nothing is estimated,
/// and neither document gains a channel.
#[test]
fn model_files_arriving_while_another_document_is_active_estimate_nothing() {
    for arrival in ["download", "local files"] {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(dir.path());
        let started_on = active_doc(&app).unwrap();
        start(&mut app);
        app.run("file.new", json!({"width": 16, "height": 16})).unwrap();
        app.session.ai.estimator = Some(Arc::new(|_, _, _, _| Err("not run".into())));
        assert!(apply(&mut app, adopted()).is_none(), "{arrival}");
        assert_eq!(inspect(&app)["phase"], "failed", "{arrival}");
        assert!(app.session.active().unwrap().doc.channels.is_empty(), "{arrival}");
        // The document that asked was not touched either.
        app.run("document.activate", json!({"document": 0})).unwrap();
        assert_eq!(active_doc(&app), Some(started_on), "{arrival}");
        assert!(app.session.active().unwrap().doc.channels.is_empty(), "{arrival}");
    }
}

/// The channel and quality are the ones asked for when Estimate Depth was chosen, even if the
/// brush settings change while the model files arrive.
#[test]
fn the_estimate_keeps_the_channel_and_quality_it_was_asked_with() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    app.session.tools.brush.surface.depth_channel = "Near".into();
    app.session.tools.brush.surface.depth_high_quality = false;
    start(&mut app);
    app.session.tools.brush.surface.depth_channel = "Other".into();
    app.session.tools.brush.surface.depth_high_quality = true;
    app.session.ai.estimator = Some(Arc::new(|_, _, _, _| Err("not run".into())));
    let after = apply(&mut app, adopted()).expect("the same document estimates");
    assert_eq!(after, After::EstimateDepth(Ask { channel: "Near".into(), quality: "fast" }));
    assert!(app.ai_setup.window.is_none());
}

#[test]
fn the_native_provider_is_read_from_the_setup_option() {
    assert_eq!(native_provider("native:estimate-depth:candle", DEPTH_TOOL).as_deref(), Some("candle"));
    assert_eq!(native_provider("native:select-subject:candle", DEPTH_TOOL), None);
    assert_eq!(native_provider("local:estimate-depth:x", DEPTH_TOOL), None);
    assert_eq!(native_provider("service:openai", DEPTH_TOOL), None);
    assert_eq!(native_provider("native:estimate-depth", DEPTH_TOOL), None);
}
