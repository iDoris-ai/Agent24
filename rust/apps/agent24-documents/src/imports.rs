//! Imports (ADR-DOC-02 §5.4, §7): a complete upload becomes a new document
//! with r1, through an import job. [`worker`] does the job's work.

pub mod worker;

/// The formats slice 1 reads (README §17 #3), told by their first bytes.
#[must_use]
pub fn media_type(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
