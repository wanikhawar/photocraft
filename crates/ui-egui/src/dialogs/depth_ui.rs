//! Estimate Depth: the Surface popover's button.
//!
//! The estimate runs `image.estimateDepth`, which uses the shared Craft AI layer's `estimate-depth`
//! tool. When the tool is not set up, the shared setup window ([`super::ai_setup`]) opens for it and
//! runs the estimate when setup ends.

use crate::PhotocraftApp;

use super::ai_setup::{After, Ask, DEPTH_TOOL};

/// The Estimate Depth… button: estimate at once when the tool is ready, else open the setup window.
pub fn start(app: &mut PhotocraftApp) {
    let Some(doc) = app.session.active().map(|d| d.doc.id) else {
        app.ui.status = tl!("No document open").into();
        app.ui.status_error = true;
        return;
    };
    let after = After::EstimateDepth(Ask::now(app));
    super::ai_setup::start(app, DEPTH_TOOL, after, Some(doc));
}

/// Why the Estimate Depth button is off, for its tooltip, or `None` while it can be used.
#[cfg(not(target_arch = "wasm32"))]
pub fn unavailable_reason(app: &PhotocraftApp) -> Option<String> {
    app.session.ai.status_reason(DEPTH_TOOL)
}

#[cfg(target_arch = "wasm32")]
pub fn unavailable_reason(_app: &PhotocraftApp) -> Option<String> {
    Some(tl!("Not available in this build").into())
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests;
