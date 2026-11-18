//! D2: a new PDF of pages of existing PDFs, made by moving the original
//! objects (page dictionaries, content streams, fonts, images) as they are,
//! never by drawing the pages again. PDFKit's writer, which drew them again,
//! dropped non-embedded fonts and turned text into paths: a 2-page 157 KB
//! policy came out at 1 MB with half its text gone.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use lopdf::dictionary;
use lopdf::{Document, Object, ObjectId};

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
    if spec == "all" {
        return Ok((1..=count).collect());
    }
    let mut pages = Vec::new();
    for part in spec.split(',') {
        if let Some((a, b)) = part.split_once('-') {
            let start = parse_page_number(a)?;
            let end = parse_page_number(b)?;
            if start > end {
                return Err(malformed_spec());
            }
            for n in start..=end {
                push_page(&mut pages, n, count)?;
            }
        } else {
            let n = parse_page_number(part)?;
            push_page(&mut pages, n, count)?;
        }
    }
    Ok(pages)
}

/// Writes to `out` a PDF of the pages `parts` names — each a PDF file and
/// its pages (`all` or `1-3,5`), in order — and returns its page count.
pub fn compose(parts: &[(PathBuf, String)], out: &Path) -> Result<u32, ComposeError> {
    let mut dest = Document::with_version("1.5");
    let pages_id = dest.try_new_object_id().map_err(failed)?;
    let catalog_id = dest.try_new_object_id().map_err(failed)?;
    let mut next_id = dest
        .max_id
        .checked_add(1)
        .ok_or_else(|| ComposeError::Failed("object id overflow".into()))?;
    let mut kids: Vec<ObjectId> = Vec::new();

    for (path, spec) in parts {
        let mut doc = Document::load(path).map_err(failed)?;
        if doc.is_encrypted() {
            return Err(ComposeError::Failed("encrypted PDF".into()));
        }
        let pages = doc.get_pages();
        let count = pages.len() as u32;
        let wanted = page_list(spec, count)?;
        for &n in &wanted {
            let Some(&page_id) = pages.get(&n) else {
                return Err(ComposeError::NoPage);
            };
            inherit_page_attrs(&mut doc, page_id)?;
            narrow_to_page(&mut doc, page_id)?;
        }
        doc.renumber_objects_with(next_id);
        next_id = doc
            .max_id
            .checked_add(1)
            .ok_or_else(|| ComposeError::Failed("object id overflow".into()))?;
        let pages = doc.get_pages();
        let selected: Vec<ObjectId> = wanted
            .iter()
            .map(|n| pages.get(n).copied().ok_or(ComposeError::NoPage))
            .collect::<Result<Vec<_>, _>>()?;

        for (id, object) in doc.objects {
            match object.type_name().unwrap_or(b"") {
                b"Catalog" | b"Pages" | b"Outlines" | b"Outline" => {}
                b"Page" => {
                    if selected.contains(&id) {
                        let mut page = object;
                        if let Ok(dict) = page.as_dict_mut() {
                            dict.set("Parent", pages_id);
                        }
                        dest.objects.insert(id, page);
                    }
                }
                _ => {
                    dest.objects.insert(id, object);
                }
            }
        }
        kids.extend(selected);
    }

    if kids.is_empty() {
        return Err(ComposeError::Failed("no pages".into()));
    }

    dest.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids.iter().copied().map(Object::Reference).collect::<Vec<Object>>(),
            "Count" => kids.len() as u32,
        }),
    );
    dest.objects.insert(
        catalog_id,
        Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        }),
    );
    dest.trailer.set("Root", catalog_id);
    dest.max_id = dest.max_id.max(next_id.saturating_sub(1));
    dest.prune_objects();
    dest.save(out).map_err(failed)?;
    Ok(kids.len() as u32)
}

fn parse_page_number(s: &str) -> Result<u32, ComposeError> {
    if s.is_empty() || s.starts_with('0') || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(malformed_spec());
    }
    s.parse::<u32>().map_err(|_| malformed_spec())
}

fn push_page(pages: &mut Vec<u32>, n: u32, count: u32) -> Result<(), ComposeError> {
    if n > count {
        return Err(ComposeError::NoPage);
    }
    pages.push(n);
    Ok(())
}

fn malformed_spec() -> ComposeError {
    ComposeError::Failed("malformed page spec".into())
}

fn failed(err: impl std::fmt::Display) -> ComposeError {
    ComposeError::Failed(err.to_string())
}

/// Copy `/Resources`, `/MediaBox`, `/CropBox`, `/Rotate` from an ancestor
/// Pages node onto the page when the page dictionary itself has none.
fn inherit_page_attrs(doc: &mut Document, page_id: ObjectId) -> Result<(), ComposeError> {
    const KEYS: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];
    for key in KEYS {
        if doc.get_dictionary(page_id).map_err(failed)?.has(key) {
            continue;
        }
        if let Some(value) = ancestor_attr(doc, page_id, key) {
            doc.get_dictionary_mut(page_id)
                .map_err(failed)?
                .set(key.to_vec(), value);
        }
    }
    Ok(())
}

/// PR-Daemon #1909 ②: what a page takes along is only what it uses. Its
/// `/Resources` (often shared by every page at the Pages node) keeps of
/// `/XObject` only the names its content draws with `Do`, written as the
/// page's own dictionary, the shared one left as it is; its form widgets drop
/// `/Parent`, the field whose `/Kids` lead to the widgets of other pages. The
/// output has no AcroForm (the catalog is new), so a widget keeps only its
/// appearance. Unreachable objects then go in `prune_objects`. A content
/// stream that cannot be read leaves the resources as they were.
fn narrow_to_page(doc: &mut Document, page_id: ObjectId) -> Result<(), ComposeError> {
    let content = doc.get_page_content(page_id);
    if let Ok(ops) = lopdf::content::Content::decode(&content) {
        let used: HashSet<Vec<u8>> = ops
            .operations
            .iter()
            .filter(|op| op.operator == "Do")
            .filter_map(|op| op.operands.first())
            .filter_map(|name| name.as_name().ok().map(<[u8]>::to_vec))
            .collect();
        let resources = doc
            .get_dictionary(page_id)
            .map_err(failed)?
            .get(b"Resources")
            .ok()
            .cloned();
        if let Some(resources) = resources {
            let mut resources = match doc.dereference(&resources) {
                Ok((_, Object::Dictionary(dict))) => dict.clone(),
                _ => return Ok(()),
            };
            if let Ok(xobjects) = resources.get(b"XObject").cloned()
                && let Ok((_, Object::Dictionary(all))) = doc.dereference(&xobjects)
            {
                let mut kept = lopdf::Dictionary::new();
                for (name, value) in all.iter() {
                    if used.contains(name) {
                        kept.set(name.clone(), value.clone());
                    }
                }
                resources.set("XObject", kept);
                doc.get_dictionary_mut(page_id)
                    .map_err(failed)?
                    .set("Resources", resources);
            }
        }
    }
    let annots: Vec<ObjectId> = match doc.get_dictionary(page_id).map_err(failed)?.get(b"Annots") {
        Ok(Object::Array(items)) => items.iter().filter_map(|o| o.as_reference().ok()).collect(),
        Ok(Object::Reference(id)) => match doc.get_object(*id) {
            Ok(Object::Array(items)) => {
                items.iter().filter_map(|o| o.as_reference().ok()).collect()
            }
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    for id in annots {
        if let Ok(annot) = doc.get_dictionary_mut(id) {
            annot.remove(b"Parent");
        }
    }
    Ok(())
}

fn ancestor_attr(doc: &Document, page_id: ObjectId, key: &[u8]) -> Option<Object> {
    let mut current = page_id;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(current) {
            return None;
        }
        let Ok(node) = doc.get_dictionary(current) else {
            return None;
        };
        if current != page_id
            && let Ok(value) = node.get(key)
        {
            return Some(value.clone());
        }
        let Ok(parent) = node.get(b"Parent") else {
            return None;
        };
        let Ok(id) = parent.as_reference() else {
            return None;
        };
        current = id;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use lopdf::dictionary;
    use lopdf::{Document, Object, Stream};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct UniqueDir(PathBuf);

    impl UniqueDir {
        fn new() -> Self {
            for i in 0..1000u32 {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path = std::env::temp_dir().join(format!(
                    "a24-pdf-compose-{}-{nanos}-{i}",
                    std::process::id()
                ));
                if std::fs::create_dir(&path).is_ok() {
                    return Self(path);
                }
            }
            panic!("could not create a unique temp directory");
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for UniqueDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn three_page_pdf(path: &Path) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources = dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        };
        let mut kids = Vec::new();
        for i in 1..=3 {
            let content = format!("BT /F1 12 Tf 72 720 Td (PAGE-{i}) Tj ET");
            let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
            });
            kids.push(Object::Reference(page_id));
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => 3u32,
                "Resources" => resources,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        doc.save(path).unwrap();
    }

    fn page_stream(doc: &Document, n: u32) -> String {
        let id = *doc.get_pages().get(&n).unwrap();
        String::from_utf8_lossy(&doc.get_page_content(id)).into_owned()
    }

    #[test]
    fn page_list_all_and_ranges() {
        assert_eq!(page_list("all", 3).unwrap(), vec![1, 2, 3]);
        assert_eq!(page_list("all", 0).unwrap(), Vec::<u32>::new());
        assert_eq!(page_list("1-3,5", 5).unwrap(), vec![1, 2, 3, 5]);
        assert_eq!(page_list("4", 3), Err(ComposeError::NoPage));
        assert_eq!(page_list("1-3,5", 4), Err(ComposeError::NoPage));
    }

    #[test]
    fn page_list_rejects_malformed_specs() {
        for spec in ["0", "3-1", "1,,2", "a"] {
            assert!(
                matches!(page_list(spec, 5), Err(ComposeError::Failed(_))),
                "{spec}"
            );
        }
    }

    #[test]
    fn split_keeps_streams_and_inherits_resources() {
        let dir = UniqueDir::new();
        let src = dir.path().join("src.pdf");
        let out = dir.path().join("split.pdf");
        three_page_pdf(&src);

        assert_eq!(compose(&[(src.clone(), "2-3".into())], &out).unwrap(), 2);
        let doc = Document::load(&out).unwrap();
        assert_eq!(doc.get_pages().len(), 2);
        let first = page_stream(&doc, 1);
        let second = page_stream(&doc, 2);
        assert!(first.contains("PAGE-2"), "{first}");
        assert!(second.contains("PAGE-3"), "{second}");
        for n in 1..=2 {
            let id = *doc.get_pages().get(&n).unwrap();
            assert!(
                doc.get_dictionary(id).unwrap().has(b"Resources"),
                "page {n} missing inherited Resources"
            );
        }
        let bytes = std::fs::read(&out).unwrap();
        assert!(
            !bytes.windows(b"PAGE-1".len()).any(|w| w == b"PAGE-1"),
            "split file still contains PAGE-1"
        );
    }

    #[test]
    fn merge_repeats_pages_in_order() {
        let dir = UniqueDir::new();
        let src = dir.path().join("src.pdf");
        let out = dir.path().join("merged.pdf");
        three_page_pdf(&src);

        assert_eq!(
            compose(
                &[(src.clone(), "all".into()), (src.clone(), "1".into()),],
                &out
            )
            .unwrap(),
            4
        );
        let doc = Document::load(&out).unwrap();
        assert_eq!(doc.get_pages().len(), 4);
        assert!(page_stream(&doc, 1).contains("PAGE-1"));
        assert!(page_stream(&doc, 2).contains("PAGE-2"));
        assert!(page_stream(&doc, 3).contains("PAGE-3"));
        assert!(page_stream(&doc, 4).contains("PAGE-1"));
    }

    #[test]
    fn compose_rejects_out_of_range_and_non_pdf() {
        let dir = UniqueDir::new();
        let src = dir.path().join("src.pdf");
        let out = dir.path().join("out.pdf");
        three_page_pdf(&src);
        assert_eq!(
            compose(&[(src, "4".into())], &out),
            Err(ComposeError::NoPage)
        );

        let text = dir.path().join("not.pdf");
        std::fs::write(&text, b"this is not a pdf").unwrap();
        assert!(matches!(
            compose(&[(text, "all".into())], &out),
            Err(ComposeError::Failed(_))
        ));
    }

    /// Three pages, one image each, all named in one `/Resources` at the
    /// Pages node; pages 1 and 2 each carry a widget of one shared field.
    fn shared_images_and_form_pdf(path: &Path) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let field_id = doc.new_object_id();
        let mut xobjects = lopdf::Dictionary::new();
        for i in 1..=3 {
            let image = Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
                    "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
                },
                format!("IMG-BYTES-{i}").into_bytes(),
            );
            xobjects.set(format!("IMG{i}"), doc.add_object(image));
        }
        let mut kids = Vec::new();
        let mut widgets = Vec::new();
        for i in 1..=3 {
            let content = format!("q 10 0 0 10 0 0 cm /IMG{i} Do Q");
            let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
            let page_id = doc.new_object_id();
            let mut page =
                dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => content_id };
            if i <= 2 {
                let ap = doc.add_object(Stream::new(
                    dictionary! {},
                    format!("WIDGET-AP-{i}").into_bytes(),
                ));
                let widget = doc.add_object(dictionary! {
                    "Type" => "Annot", "Subtype" => "Widget", "Parent" => field_id, "P" => page_id,
                    "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
                    "AP" => dictionary! { "N" => ap },
                });
                widgets.push(Object::Reference(widget));
                page.set("Annots", vec![Object::Reference(widget)]);
            }
            doc.objects.insert(page_id, Object::Dictionary(page));
            kids.push(Object::Reference(page_id));
        }
        doc.objects.insert(
            field_id,
            Object::Dictionary(dictionary! { "FT" => "Tx", "T" => "name", "Kids" => widgets }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => kids, "Count" => 3u32,
                "Resources" => dictionary! { "XObject" => xobjects },
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        doc.save(path).unwrap();
    }

    #[test]
    fn split_takes_only_the_images_and_widgets_of_its_pages() {
        let dir = UniqueDir::new();
        let src = dir.path().join("src.pdf");
        let out = dir.path().join("one.pdf");
        shared_images_and_form_pdf(&src);
        compose(&[(src, "1".into())], &out).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        let has = |needle: &str| bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
        assert!(
            has("IMG-BYTES-1") && has("WIDGET-AP-1"),
            "page 1's own image and widget"
        );
        for gone in ["IMG-BYTES-2", "IMG-BYTES-3", "WIDGET-AP-2"] {
            assert!(!has(gone), "{gone} came along with page 1");
        }
    }

    #[test]
    fn split_one_page_is_not_larger_than_source() {
        let dir = UniqueDir::new();
        let src = dir.path().join("src.pdf");
        let out = dir.path().join("one.pdf");
        three_page_pdf(&src);
        compose(&[(src.clone(), "1".into())], &out).unwrap();
        let orig = std::fs::metadata(&src).unwrap().len();
        let split = std::fs::metadata(&out).unwrap().len();
        assert!(
            split <= orig,
            "1-page file {split} bytes grew past 3-page source {orig}"
        );
    }
}
