//! D2: a new PDF of pages of existing PDFs, made by moving the original
//! objects (page dictionaries, content streams, fonts, images) as they are,
//! never by drawing the pages again. PDFKit's writer, which drew them again,
//! dropped non-embedded fonts and turned text into paths: a 2-page 157 KB
//! policy came out at 1 MB with half its text gone.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ComposeError {
    /// A page past an input's last page.
    #[error("a page is past the document's last page")]
    NoPage,
    /// An input that is not a readable PDF (or is encrypted), or an output
    /// that could not be written.
    #[error("{0}")]
    Failed(String),
}

/// `all`, or 1-based pages and ranges like `1-3,5`, as the pages of a
/// document with `count` pages, in the order given. A page past `count` is
/// [`ComposeError::NoPage`]; a malformed spec is `Failed` (the handler
/// checks the form first, `compose::pages_ok`).
pub fn page_list(spec: &str, count: u32) -> Result<Vec<u32>, ComposeError> {
    let _ = (spec, count);
    Err(ComposeError::Failed("not implemented".into()))
}

/// Writes to `out` a PDF of the pages `parts` names — each a PDF file and
/// its pages (`all` or `1-3,5`), in order — and returns its page count.
pub fn compose(parts: &[(PathBuf, String)], out: &Path) -> Result<u32, ComposeError> {
    let _ = (parts, out);
    Err(ComposeError::Failed("not implemented".into()))
}
