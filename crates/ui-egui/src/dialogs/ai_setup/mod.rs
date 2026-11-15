//! Setting up a shared AI tool from a button: Estimate Depth (Surface options) and Stay on
//! Object's AI object finder (Select Subject).
//!
//! A tool runs on the shared Craft AI layer. When it is not set up, [`native`] draws the shared
//! layer's own screens over its state (the missing-model popup, then
//! `craft_ai_egui::native_model_setup` over a `SetupSession`), so setup, the download and the
//! consent to it are the shared flow, not a second one here. The window takes the tool id and what
//! follows setup ([`After`]): Estimate Depth then runs `image.estimateDepth` with the settings it
//! was asked with; Stay on Object runs nothing, it only forgets the tool's status so the next
//! stroke uses the model. Everything that reads or writes disk or the network runs on a worker
//! thread. The web build has no AI layer: the buttons are disabled there.

use crate::PhotocraftApp;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

#[cfg(not(target_arch = "wasm32"))]
pub use photocraft_engine::ai_layer::{DEPTH_TOOL, SELECT_SUBJECT_TOOL};
/// The catalogue ids of the tools (the engine's, which the web build does not have).
#[cfg(target_arch = "wasm32")]
pub const DEPTH_TOOL: &str = "estimate-depth";
#[cfg(target_arch = "wasm32")]
pub const SELECT_SUBJECT_TOOL: &str = "select-subject";

/// What one Estimate Depth asks for: the channel to write and the quality, fixed when it is chosen.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub channel: String,
    pub quality: &'static str,
}

impl Ask {
    /// The brush's current Estimate Depth settings.
    pub fn now(app: &PhotocraftApp) -> Self {
        let surface = &app.session.tools.brush.surface;
        Self { channel: surface.depth_channel.clone(), quality: if surface.depth_high_quality { "high" } else { "fast" } }
    }
}

/// What runs when a tool's setup ends.
#[derive(Clone, Debug, PartialEq)]
pub enum After {
    /// Nothing: the tool's status is forgotten, and whatever asked uses the model next time.
    Nothing,
    /// Run `image.estimateDepth` with these settings.
    EstimateDepth(Ask),
}

/// The "Set Up AI Object Finding…" button of Stay on Object: no document, nothing runs afterwards.
pub fn start_object_finding(app: &mut PhotocraftApp) {
    start(app, SELECT_SUBJECT_TOOL, After::Nothing, None);
}

#[cfg(target_arch = "wasm32")]
mod web {
    use photocraft_doc::DocId;
    use serde_json::Value;

    use super::After;
    use crate::PhotocraftApp;

    /// Nothing to keep: there is no setup flow on the web.
    #[derive(Default)]
    pub struct AiSetup;

    pub fn start(app: &mut PhotocraftApp, _tool: &'static str, _after: After, _doc: Option<DocId>) {
        app.ui.status = tl!("Not available in this build").into();
        app.ui.status_error = true;
    }

    pub fn show(_app: &mut PhotocraftApp, _ctx: &egui::Context) {}

    /// There is no setup window to close.
    pub fn cancel_stale(_app: &mut PhotocraftApp) {}

    pub fn cancel(_app: &mut PhotocraftApp) {}

    pub fn inspect(_app: &PhotocraftApp) -> Value {
        Value::Null
    }
}
#[cfg(target_arch = "wasm32")]
pub use web::*;

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests;
