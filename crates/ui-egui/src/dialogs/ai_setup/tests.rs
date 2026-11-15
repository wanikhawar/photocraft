//! The shared AI setup window for Stay on Object's object finder (Select Subject): the window is
//! for that tool, needs no document, and runs nothing when setup ends. Estimate Depth's own flow is
//! tested in `dialogs::depth_ui`.

use std::sync::Arc;

use photocraft_engine::ai_layer::{AiHost, DEPTH_TOOL, SELECT_SUBJECT_TOOL};
use serde_json::json;

use super::*;
use crate::PhotocraftApp;

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

/// Draw frames of the window until `ui.inspect` reports `phase` (workers answer in between).
fn wait_for_phase(app: &mut PhotocraftApp, phase: &str) {
    let ctx = egui::Context::default();
    for _ in 0..400 {
        ctx.run_ui(egui::RawInput::default(), |ui| show(app, ui.ctx())).textures_delta.clear();
        if inspect(app)["phase"] == phase {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("the window never reached `{phase}`: {}", inspect(app));
}

#[test]
fn setting_up_object_finding_shows_the_popup_for_select_subject_and_runs_no_estimate() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    start_object_finding(&mut app);
    let w = app.ai_setup.window.as_ref().expect("the window opens: nothing is set up");
    assert_eq!(w.tool, SELECT_SUBJECT_TOOL);
    assert!(w.doc.is_none(), "it belongs to no document");
    assert_eq!(w.after, After::Nothing);
    wait_for_phase(&mut app, "missing");
    let Some(Stage::Missing(popup)) = app.ai_setup.window.as_ref().map(|w| &w.stage) else { panic!("the missing-model popup") };
    assert_eq!(popup.tool.as_str(), SELECT_SUBJECT_TOOL, "the popup is for the object finder, not for Estimate Depth");

    // The window outlives the document that was active: it has none to wait for.
    app.session.execute("file.close", json!({})).unwrap();
    cancel_stale(&mut app);
    assert!(app.ai_setup.window.is_some());

    // The tool becomes ready (a stand-in answers); finishing closes the window and runs nothing.
    app.session.ai.segmenter = Some(Arc::new(|_, _, _, _| Err("not run".into())));
    app.run("file.new", json!({"width": 16, "height": 16})).unwrap();
    app.session.ai.estimator = Some(Arc::new(|_, _, _, _| Err("estimate must not run".into())));
    let done = apply(&mut app, Msg::Chosen(Ok(())));
    assert_eq!(done, Some(After::Nothing));
    assert!(app.ai_setup.window.is_none());
    assert!(app.session.active().unwrap().doc.channels.is_empty(), "no depth estimate ran");
    assert!(!app.ui.status_error, "{}", app.ui.status);
}

#[test]
fn a_window_open_for_one_tool_is_not_replaced_by_another() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    start_object_finding(&mut app);
    crate::dialogs::depth_ui::start(&mut app);
    assert_eq!(app.ai_setup.window.as_ref().map(|w| w.tool), Some(SELECT_SUBJECT_TOOL), "the running setup is kept");
    // Once it failed, the other tool's button takes the window.
    if let Some(w) = app.ai_setup.window.as_mut() {
        w.stage = Stage::Failed;
    }
    crate::dialogs::depth_ui::start(&mut app);
    assert_eq!(app.ai_setup.window.as_ref().map(|w| w.tool), Some(DEPTH_TOOL));
}

#[test]
fn the_setup_option_must_name_the_tool_being_set_up() {
    assert_eq!(native_provider("native:select-subject:candle", SELECT_SUBJECT_TOOL).as_deref(), Some("candle"));
    assert_eq!(native_provider("native:estimate-depth:candle", SELECT_SUBJECT_TOOL), None);
}
