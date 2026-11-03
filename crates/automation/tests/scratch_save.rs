//! A scripted `.pcraft` save refuses to write previews or a full-size composite rendered from
//! scratch tiles that could not be read. Its own test binary: the scratch store is per process.
#![cfg(not(target_arch = "wasm32"))]

use photocraft_automation::{AutomationError, files};
use photocraft_doc::{ColorMode, Document, Layer, PixelFormat, Rect, SampleType, Size};
use photocraft_geom::TileCoord;
use photocraft_io::{BandRenderer, IoError, RenderBand, RenderError};
use photocraft_raster::{Interrupt, spill};
use std::io::{Read, Seek, SeekFrom, Write};

struct QueuedDiagnosticsRenderer(Vec<String>);

/// Renders through the reference compositor, then makes the scratch copy of every tile unreadable
/// so the final health check fails after the export looked successful.
struct CorruptingRenderer {
    diagnostics: Vec<String>,
}

impl BandRenderer for CorruptingRenderer {
    fn render_bands(
        &mut self,
        doc: &Document,
        rect: Rect,
        band_rows: i32,
        ctl: &Interrupt<'_>,
        sink: &mut dyn FnMut(RenderBand) -> Result<(), IoError>,
    ) -> Result<(), RenderError> {
        let mut cpu = photocraft_io::CpuRenderer;
        cpu.render_bands(doc, rect, band_rows, ctl, sink)?;
        self.diagnostics.push("adapter status before the scratch failure".into());
        // The compositor renders bands and tiles in parallel, so it can leave the tiles it just
        // read in memory, and a resident tile never checks its scratch copy. Evict everything
        // first (the tiles then live only on the scratch disk, as the other scratch tests arrange
        // before damaging it), then damage the whole file: whether a read fails must not depend on
        // which tile the last worker happened to leave resident.
        spill::settle();
        if let Some(mut file) = spill::scratch_handle_for_tests() {
            file.seek(SeekFrom::Start(0)).unwrap();
            // At least the extents written so far, and never less than the old fixed window.
            let len = usize::try_from(file.metadata().map(|m| m.len()).unwrap_or(0)).unwrap_or(0).max(1 << 20);
            file.write_all(&vec![0u8; len]).unwrap();
            file.sync_all().unwrap();
        }
        let mut failed = false;
        for layer in &doc.layers {
            if let Some(surface) = layer.surface() {
                for (_, tile) in surface.tiles() {
                    failed |= tile.try_bytes().is_err();
                }
            }
        }
        assert!(failed, "the corrupted scratch file must make at least one tile unreadable once it is evicted");
        Ok(())
    }

    fn drain_diagnostics(&mut self) -> Vec<String> {
        std::mem::take(&mut self.diagnostics)
    }

    fn push_diagnostic(&mut self, diagnostic: String) {
        self.diagnostics.push(diagnostic);
    }
}

impl BandRenderer for QueuedDiagnosticsRenderer {
    fn render_bands(
        &mut self,
        _doc: &Document,
        _rect: Rect,
        _band_rows: i32,
        _ctl: &Interrupt<'_>,
        _sink: &mut dyn FnMut(RenderBand) -> Result<(), IoError>,
    ) -> Result<(), RenderError> {
        Ok(())
    }

    fn drain_diagnostics(&mut self) -> Vec<String> {
        std::mem::take(&mut self.0)
    }

    fn push_diagnostic(&mut self, diagnostic: String) {
        self.0.push(diagnostic);
    }
}

#[test]
fn native_save_refuses_previews_rendered_from_unreadable_scratch() {
    let dir = std::env::temp_dir().join(format!("photocraft-automation-scratch-save-{}", std::process::id()));
    spill::configure(Some(&dir), 1).unwrap();
    let mut doc = Document::new("Scratch", Size::new(512, 256), ColorMode::Rgb, SampleType::U8);
    let mut layer = Layer::raster("Pixels", PixelFormat::RGBA8);
    for i in 0..2 {
        let mut bytes = layer.surface_mut().unwrap().tile_mut(TileCoord::new(i, 0)).bytes_mut();
        for (j, byte) in bytes.iter_mut().enumerate() {
            *byte = (j as u8).wrapping_mul(29).wrapping_add((i * 7) as u8 + 1);
        }
    }
    doc.layers.push(layer);
    spill::settle();
    assert_eq!(spill::stats().resident_bytes, 0);
    let mut file = spill::scratch_handle_for_tests().unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut good = Vec::new();
    file.read_to_end(&mut good).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&vec![0; good.len()]).unwrap();
    file.sync_all().unwrap();

    let on = photocraft_io::ExportOptions { pcraft_full_composite: true, ..Default::default() };
    // Refused while rendering the previews, before the bundle is assembled.
    let err = files::save_bytes(&doc, "Scratch.pcraft", None, &on).unwrap_err();
    assert!(matches!(err, AutomationError::Io(_)), "{err:?}");
    // A failed native encode must leave the renderer's queued diagnostics for the caller.
    let mut renderer = QueuedDiagnosticsRenderer(vec!["queued native save diagnostic".into()]);
    let err = files::save_bytes_with_renderer(&doc, "Scratch.pcraft", None, &on, Some(&mut renderer)).unwrap_err();
    assert!(matches!(err, AutomationError::Io(_)), "{err:?}");
    assert_eq!(renderer.0, ["queued native save diagnostic"], "a failed native save must leave queued diagnostics on the renderer");
    let path = dir.join("Scratch.pcraft");
    let err = files::save(&doc, &path, None, &on, None).unwrap_err();
    assert!(matches!(err, AutomationError::Io(_)), "{err:?}");
    assert!(!path.exists());

    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&good).unwrap();
    file.sync_all().unwrap();
    for (_, tile) in doc.layers[0].surface().unwrap().tiles() {
        tile.try_bytes().unwrap();
    }
    assert!(spill::check_integrity().is_ok());
    let (bytes, _) = files::save_bytes(&doc, "Scratch.pcraft", None, &on).unwrap();
    assert_eq!(photocraft_format::load_from_bytes(&bytes).unwrap().layers, doc.layers);

    // A flat export that succeeds provisionally keeps its renderer diagnostics when the final
    // scratch check fails afterwards.
    doc.layers.push(doc.layers[0].clone());
    spill::settle();
    let mut renderer = CorruptingRenderer { diagnostics: Vec::new() };
    let mut diagnostics = Vec::new();
    let err =
        files::save_bytes_with_renderer_diagnostics(&doc, "Scratch.png", Some("png"), &Default::default(), Some(&mut renderer), &mut diagnostics).unwrap_err();
    assert!(matches!(err, AutomationError::Io(_) | AutomationError::Import(_)), "{err:?}");
    assert!(
        diagnostics.iter().any(|d| d.contains("adapter status before the scratch failure")),
        "a final scratch failure must keep the provisional renderer diagnostics: {diagnostics:?}"
    );

    drop((doc, file));
    spill::configure(None, 0).unwrap();
    let _ = std::fs::remove_dir_all(dir);
}
