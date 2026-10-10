#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::text_layer::ParseStatus;

const CONTENT: &str = "sha256:4f26fc6d4e157f4c2eb6c72b4faa687467ea3331634a21c65fc4b1162c6008ce";

/// A stand-in helper: a shell script with `body`, in `dir`.
fn helper(dir: &Path, body: &str) -> PdfKit {
    let path = dir.join(HELPER);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    PdfKit::at(path, "26.6.2")
}

fn report(os: &str) -> String {
    json!({ "protocol": 1, "os_version": os, "unparsed": [],
        "pages": [{ "page": 1, "width": 612, "height": 792,
            "lines": [{ "text": "通告", "rect": [72, 100, 200, 112] }] }] })
    .to_string()
}

async fn parse(e: &PdfKit) -> Result<crate::text_layer::TextLayer, EngineError> {
    e.parse(CONTENT, "application/pdf", Path::new("/x.pdf"))
        .await
}

#[tokio::test]
async fn a_report_becomes_a_layer_of_this_engine_and_this_content() {
    let dir = tempfile::tempdir().unwrap();
    let e = helper(dir.path(), &format!("cat <<'J'\n{}\nJ", report("26.6.2")));
    let layer = parse(&e).await.unwrap();
    assert_eq!(
        (layer.content_sha256.as_str(), layer.engine.clone()),
        (CONTENT, e.engine())
    );
    assert_eq!(
        (layer.config.clone(), layer.parse_status),
        (e.config(), ParseStatus::Complete)
    );
    assert_eq!(layer.blocks[0].text, "通告");
}

#[tokio::test]
async fn a_version_without_its_zero_patch_is_the_same_macos() {
    let dir = tempfile::tempdir().unwrap();
    let path = helper(dir.path(), "true").path;
    std::fs::write(
        &path,
        format!("#!/bin/sh\ncat <<'J'\n{}\nJ\n", report("26.6.0")),
    )
    .unwrap();
    // `sw_vers` says "26.6"; the helper says "26.6.0".
    let e = PdfKit::at(path, "26.6");
    assert_eq!(e.engine().version, "26.6.0");
    assert!(parse(&e).await.is_ok());
    assert_eq!(canonical("26.6.2").as_deref(), Some("26.6.2"));
    assert_eq!(canonical("26"), None);
    assert_eq!(canonical("26.x"), None);
}

#[tokio::test]
async fn the_helper_gets_the_file_and_its_media_type() {
    let dir = tempfile::tempdir().unwrap();
    // Exits 2 (unsupported) unless called as `parse /x.pdf application/pdf`.
    let e = helper(
        dir.path(),
        &format!(
            "[ \"$1 $2 $3\" = 'parse /x.pdf application/pdf' ] || exit 2\ncat <<'J'\n{}\nJ",
            report("26.6.2")
        ),
    );
    assert!(parse(&e).await.is_ok());
}

#[tokio::test]
async fn each_way_a_run_can_fail_is_the_engine_failing() {
    let dir = tempfile::tempdir().unwrap();
    let cases: Vec<(&str, String)> = vec![
        ("exit 2", "exit 2".into()),
        ("exit 3", "echo 'the PDF is encrypted' >&2; exit 3".into()),
        ("a signal", "kill -9 $$".into()),
        ("not JSON", "echo nope".into()),
        ("another macOS", format!("cat <<'J'\n{}\nJ", report("26.7"))),
        (
            "not a layer",
            "echo '{\"protocol\":2,\"os_version\":\"26.6.2\",\"pages\":[],\"unparsed\":[]}'".into(),
        ),
    ];
    for (why, body) in cases {
        let got = parse(&helper(dir.path(), &body)).await;
        match (why, &got) {
            ("exit 2", Err(EngineError::Unsupported)) => {}
            ("exit 3", Err(EngineError::Failed(m))) => {
                assert!(m.contains("the PDF is encrypted"), "{m}")
            }
            ("exit 2", _) | ("exit 3", _) => panic!("{why}: {got:?}"),
            (_, Err(EngineError::Failed(_))) => {}
            _ => panic!("{why}: {got:?}"),
        }
    }
    let missing = PdfKit::at(dir.path().join("nowhere"), "26.6.2");
    assert!(matches!(parse(&missing).await, Err(EngineError::Failed(_))));
}

#[tokio::test]
async fn a_helper_that_runs_too_long_or_says_too_much_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let slow = helper(dir.path(), "sleep 30").limited(Duration::from_millis(200), MAX_OUTPUT);
    let start = std::time::Instant::now();
    assert!(matches!(parse(&slow).await, Err(EngineError::Failed(m)) if m.contains("too long")));
    assert!(start.elapsed() < Duration::from_secs(5));
    // A report over the cap stops the helper at once, even one that would
    // live on once its output is closed (it ignores SIGPIPE).
    let loud = helper(dir.path(), "trap '' PIPE; yes 2>/dev/null; exec sleep 30")
        .limited(Duration::from_secs(10), 1000);
    let start = std::time::Instant::now();
    assert!(matches!(parse(&loud).await, Err(EngineError::Failed(m)) if m.contains("too large")));
    assert!(start.elapsed() < Duration::from_secs(5));
}

/// The real helper, built and run on S01 samples (macOS only: CI runs this
/// on its macOS runner).
#[cfg(target_os = "macos")]
#[tokio::test]
async fn the_real_helper_reads_the_s01_samples() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = tempfile::tempdir().unwrap();
    let built = std::process::Command::new(root.join("engines/pdfkit/build.sh"))
        .arg(out.path())
        .status()
        .unwrap();
    assert!(built.success(), "the helper did not build");
    let os = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .unwrap();
    let e = PdfKit::at(
        out.path().join(HELPER),
        String::from_utf8(os.stdout).unwrap().trim(),
    );
    let samples = root.join("../../../docs/documenting/samples/s01");
    for (sample, media, pages, text) in [
        (
            "s01-02-en-epa-boil-water/source.pdf",
            "application/pdf",
            3,
            "BOIL YOUR WATER BEFORE USING",
        ),
        (
            "s01-03-zh-holiday-2026/source.pdf",
            "application/pdf",
            2,
            "国务院办公厅关于2026年",
        ),
        (
            "s01-07-mixed-building-notice/source.pdf",
            "application/pdf",
            1,
            "Water shut-off",
        ),
        (
            "s01-06-zh-holiday-2026-scan/source.jpg",
            "image/jpeg",
            1,
            "国务院办公厅",
        ),
    ] {
        let layer = e
            .parse(CONTENT, media, &samples.join(sample))
            .await
            .unwrap();
        assert_eq!(layer.pages, pages, "{sample}");
        assert!(
            layer.blocks.iter().any(|b| b.text.contains(text)),
            "{sample}: {text}"
        );
    }
    let unsupported = e
        .parse(
            CONTENT,
            "image/png",
            &samples.join("s01-02-en-epa-boil-water/source.pdf"),
        )
        .await;
    assert_eq!(unsupported.err(), Some(EngineError::Unsupported));
}
