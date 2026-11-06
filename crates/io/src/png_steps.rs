//! A PNG import in resumable steps, for a caller that must not block for the whole file (the web
//! build has no threads): the same document [`crate::import_with_options_and_interrupt`] gives.

use std::sync::Arc;

use photocraft_codecs::{self as codecs, DecodeOptions, Format, Limits, PngDecodeRows, SharedBytes};

use crate::flat::{FlatBuilder, PngTiles};
use crate::{ImportResult, IoError, Route};

enum Stage {
    Rows(Box<PngDecodeRows<SharedBytes>>),
    Stream(Box<PngDecodeRows<SharedBytes>>, Box<PngTiles>),
    Prune(Box<PngTiles>, Box<codecs::PngExtras>),
    Orient(Box<codecs::orientation::PngOrientation>),
    /// The decoded image becoming the document, a band of rows and a batch of tiles at a time.
    Build(Box<FlatBuilder>),
    Spent,
}

/// A plain PNG file being imported as a document, a few rows at a time.
pub struct PngImport {
    name: String,
    bytes: Arc<Vec<u8>>,
    stage: Stage,
    limits: Limits,
}

impl PngImport {
    /// Starts importing `bytes` (named `name`) when the import would decode them as a plain PNG,
    /// which reads the header and refuses what the import refuses; `None` for any other file,
    /// which only [`crate::import_with_options_and_interrupt`] reads. The decoder shares `bytes`.
    pub fn new(name: &str, bytes: &Arc<Vec<u8>>) -> Result<Option<Self>, IoError> {
        Self::with_options(name, bytes, &crate::ImportOptions::default())
    }

    /// Starts a stepped import with the same flat decode limits as a regular import.
    pub fn with_options(name: &str, bytes: &Arc<Vec<u8>>, opts: &crate::ImportOptions) -> Result<Option<Self>, IoError> {
        if crate::route(name, bytes) != Route::Flat || codecs::detect(bytes) != Some(Format::Png) {
            return Ok(None);
        }
        let limits = crate::flat::decode_limits(opts);
        let input = || SharedBytes(Arc::clone(bytes));
        let stage = if let Some(rows) = PngDecodeRows::streaming(input(), &limits)? {
            let tiles = PngTiles::new(rows.header())?;
            Stage::Stream(Box::new(rows), Box::new(tiles))
        } else {
            Stage::Rows(Box::new(PngDecodeRows::new(input(), &limits)?))
        };
        Ok(Some(Self { name: name.to_string(), bytes: Arc::clone(bytes), stage, limits }))
    }

    /// How much of the import is done, `0.0..=1.0`.
    pub fn progress(&self) -> f32 {
        match &self.stage {
            Stage::Rows(rows) | Stage::Stream(rows, _) => rows.progress(),
            Stage::Prune(_, _) => 1.0,
            Stage::Orient(_) | Stage::Build(_) | Stage::Spent => 1.0,
        }
    }

    /// Works while `more()` returns true (at least one bounded piece per call): rows, then the
    /// orientation bands, then the image's rows into the document's tiles and the prune scan. `Some` once the document is
    /// built, after which the import is spent.
    pub fn step(&mut self, more: &mut dyn FnMut() -> bool) -> Result<Option<ImportResult>, IoError> {
        match std::mem::replace(&mut self.stage, Stage::Spent) {
            Stage::Stream(mut rows, mut tiles) => {
                self.stage = match rows.stream_step(more, &mut |y, data| tiles.write(y, data))? {
                    Some(extras) if extras.orientation != 1 => {
                        // Orientation may appear after IDAT. Drop the tiles before restarting.
                        drop(tiles);
                        Stage::Rows(Box::new(PngDecodeRows::new(SharedBytes(Arc::clone(&self.bytes)), &self.limits)?))
                    }
                    Some(extras) => Stage::Prune(tiles, Box::new(extras)),
                    None => Stage::Stream(rows, tiles),
                };
                Ok(None)
            }
            Stage::Prune(mut tiles, extras) => {
                if tiles.prune_step() {
                    tiles.finish(&self.name, self.bytes.as_slice(), *extras).map(Some)
                } else {
                    self.stage = Stage::Prune(tiles, extras);
                    Ok(None)
                }
            }
            Stage::Rows(mut rows) => {
                self.stage = match rows.step(more)? {
                    Some(img) => {
                        // The resolution, upright orientation and final limit checks `decode` does.
                        Stage::Orient(Box::new(codecs::png_orientation(img, &DecodeOptions { limits: self.limits, ..Default::default() })?))
                    }
                    None => Stage::Rows(rows),
                };
                Ok(None)
            }
            Stage::Orient(mut orient) => {
                self.stage = match orient.step(more)? {
                    Some(img) => Stage::Build(Box::new(FlatBuilder::new(&self.name, img)?)),
                    None => Stage::Orient(orient),
                };
                Ok(None)
            }
            Stage::Build(mut build) => loop {
                if !build.copy_done() {
                    build.copy_step()?;
                    if !more() {
                        self.stage = Stage::Build(build);
                        return Ok(None);
                    }
                    continue;
                }
                if !build.prune_done() {
                    build.prune_step();
                    if !more() {
                        self.stage = Stage::Build(build);
                        return Ok(None);
                    }
                    continue;
                }
                return build.finish(self.bytes.as_slice(), Some(Format::Png)).map(Some);
            },
            Stage::Spent => Err(IoError::Unsupported("the PNG was already imported".into())),
        }
    }
}
