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

const PNG_BYTES: &str = r"\211PNG\r\n\032\n";

fn ask(region: Option<[f64; 4]>) -> RenderAsk {
    RenderAsk {
        page: 2,
        scale: 1.5,
        region,
        max_bytes: 1 << 20,
    }
}

async fn render(e: &PdfKit, ask: RenderAsk) -> Result<Rendered, RenderError> {
    e.render("application/pdf", Path::new("/x.pdf"), ask).await
}

#[tokio::test]
async fn a_render_gets_the_page_scale_size_and_region_and_says_the_scale_it_used() {
    let dir = tempfile::tempdir().unwrap();
    let args = dir.path().join("args");
    let e = helper(
        dir.path(),
        &format!(
            "echo \"$@\" > {}\nprintf '{{\"protocol\":1,\"scale\":1.25,\"width\":9,\"height\":9}}\\n{PNG_BYTES}IDAT'",
            args.display()
        ),
    );
    let out = render(&e, ask(Some([0.0, 10.5, 300.0, 400.25])))
        .await
        .unwrap();
    assert_eq!(out.scale, 1.25);
    assert_eq!(out.png, b"\x89PNG\r\n\x1a\nIDAT");
    assert_eq!(
        std::fs::read_to_string(&args).unwrap().trim(),
        "render /x.pdf application/pdf 2 1.5 1048576 0,10.5,300,400.25"
    );
    render(&e, ask(None)).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&args).unwrap().trim(),
        "render /x.pdf application/pdf 2 1.5 1048576"
    );
}

#[tokio::test]
async fn each_exit_of_a_render_is_its_own_error() {
    let dir = tempfile::tempdir().unwrap();
    for (body, want) in [
        ("exit 2", RenderError::Unsupported),
        ("exit 4", RenderError::NoPage),
        ("exit 5", RenderError::OffPage),
        ("exit 6", RenderError::TooLarge),
    ] {
        let e = helper(dir.path(), body);
        assert_eq!(render(&e, ask(None)).await, Err(want), "{body}");
    }
    let e = helper(dir.path(), "echo 'cannot open' >&2; exit 3");
    let failed = render(&e, ask(None)).await;
    assert!(
        matches!(&failed, Err(RenderError::Failed(m)) if m.contains("(3): cannot open")),
        "{failed:?}"
    );
    let e = helper(dir.path(), "kill -9 $$");
    assert!(
        matches!(render(&e, ask(None)).await, Err(RenderError::Failed(m)) if m.contains("stopped"))
    );
}

#[tokio::test]
async fn a_render_that_is_not_what_was_asked_for_is_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let head = |protocol: u32, scale: f64| format!("{{\"protocol\":{protocol},\"scale\":{scale}}}");
    for (what, out) in [
        ("no header", PNG_BYTES.to_string()),
        ("not json", format!("scale 1\\n{PNG_BYTES}")),
        (
            "another protocol",
            format!("{}\\n{PNG_BYTES}", head(2, 1.0)),
        ),
        (
            "above the scale asked",
            format!("{}\\n{PNG_BYTES}", head(1, 1.501)),
        ),
        (
            "below the smallest",
            format!("{}\\n{PNG_BYTES}", head(1, 0.249)),
        ),
        ("not a PNG", format!("{}\\nGIF89a", head(1, 1.0))),
    ] {
        let e = helper(dir.path(), &format!("printf '{out}'"));
        assert!(
            matches!(render(&e, ask(None)).await, Err(RenderError::Failed(_))),
            "{what}"
        );
    }
    // Over the size asked for, but within the header's allowance.
    let e = helper(
        dir.path(),
        &format!(
            "printf '{}\\n{PNG_BYTES}'; head -c 20 /dev/zero",
            head(1, 1.0)
        ),
    );
    let small = RenderAsk {
        max_bytes: 16,
        ..ask(None)
    };
    assert!(
        matches!(render(&e, small).await, Err(RenderError::Failed(m)) if m.contains("within the size"))
    );
    // Far over it: stopped as it writes.
    let e = helper(dir.path(), "head -c 100000000 /dev/zero; sleep 30");
    let start = std::time::Instant::now();
    assert!(
        matches!(render(&e, small).await, Err(RenderError::Failed(m)) if m.contains("too large"))
    );
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn a_render_that_runs_too_long_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let slow = helper(dir.path(), "sleep 30").limited(Duration::from_millis(300), 1 << 20);
    let start = std::time::Instant::now();
    assert_eq!(render(&slow, ask(None)).await, Err(RenderError::TooSlow));
    assert!(start.elapsed() < Duration::from_secs(5));
}

/// A render given up on (dropped) stops its helper.
#[tokio::test]
async fn a_render_given_up_on_stops_its_helper() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("pid");
    let e = helper(
        dir.path(),
        &format!("echo $$ > {}\nexec sleep 30", pid.display()),
    );
    // Given up once the helper is running (a first run can be slow to start).
    let running = async {
        while !pid.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::select! {
        out = render(&e, ask(None)) => panic!("the render ended: {out:?}"),
        () = running => {}
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let pid = std::fs::read_to_string(&pid).unwrap().trim().to_owned();
    // Killed, then reaped by tokio: soon no such process.
    let mut alive = true;
    for _ in 0..50 {
        let probe = std::process::Command::new("kill")
            .args(["-0", &pid])
            .output()
            .unwrap();
        alive = probe.status.success();
        if !alive {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!alive, "the helper {pid} still runs");
}

/// The real helper's renders of an S01 sample (macOS only).
#[cfg(target_os = "macos")]
#[tokio::test]
async fn the_real_helper_renders_pages_and_regions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = tempfile::tempdir().unwrap();
    let built = std::process::Command::new(root.join("engines/pdfkit/build.sh"))
        .arg(out.path())
        .status()
        .unwrap();
    assert!(built.success(), "the helper did not build");
    let e = PdfKit::at(out.path().join(HELPER), "26.6.2");
    let pdf =
        root.join("../../../docs/documenting/samples/s01/s01-02-en-epa-boil-water/source.pdf");
    let jpg =
        root.join("../../../docs/documenting/samples/s01/s01-06-zh-holiday-2026-scan/source.jpg");
    let page = |page, scale, region, max_bytes| RenderAsk {
        page,
        scale,
        region,
        max_bytes,
    };
    let whole = e
        .render("application/pdf", &pdf, page(1, 1.0, None, 1 << 20))
        .await
        .unwrap();
    assert_eq!((whole.scale, &whole.png[..8]), (1.0, PNG));
    // Lowered until it fits; the scale used is said.
    let fitted = e
        .render("application/pdf", &pdf, page(1, 4.0, None, 100_000))
        .await
        .unwrap();
    assert!(
        fitted.scale < 4.0 && fitted.png.len() <= 100_000,
        "{}",
        fitted.scale
    );
    let line = e
        .render(
            "application/pdf",
            &pdf,
            page(1, 2.0, Some([152.0, 98.0, 467.0, 119.0]), 1 << 20),
        )
        .await
        .unwrap();
    assert!(line.png.len() < whole.png.len());
    // A scale with no short decimal is used and said at most as asked.
    for scale in [0.280_999_999_999_999_97, 1.246_999_999_999_999_9] {
        let tile = e
            .render(
                "application/pdf",
                &pdf,
                page(1, scale, Some([0.0, 0.0, 20.0, 20.0]), 1 << 20),
            )
            .await
            .unwrap();
        assert!(
            tile.scale <= scale && tile.scale > scale - 0.002,
            "{scale}: {}",
            tile.scale
        );
    }
    let image = e
        .render("image/jpeg", &jpg, page(1, 1.0, None, 1 << 20))
        .await
        .unwrap();
    assert_eq!(&image.png[..8], PNG);
    for (media, file, ask, want) in [
        (
            "application/pdf",
            &pdf,
            page(4, 1.0, None, 1 << 20),
            RenderError::NoPage,
        ),
        (
            "image/jpeg",
            &jpg,
            page(2, 1.0, None, 1 << 20),
            RenderError::NoPage,
        ),
        (
            "application/pdf",
            &pdf,
            page(1, 1.0, Some([900.0, 900.0, 950.0, 950.0]), 1 << 20),
            RenderError::OffPage,
        ),
        (
            "application/pdf",
            &pdf,
            page(1, 1.0, None, 2_000),
            RenderError::TooLarge,
        ),
        (
            "image/png",
            &pdf,
            page(1, 1.0, None, 1 << 20),
            RenderError::Unsupported,
        ),
    ] {
        assert_eq!(
            e.render(media, file, ask).await.err(),
            Some(want.clone()),
            "{want:?}"
        );
    }
}

/// On a page turned by /Rotate 0, 90, 180 or 270, the region of the line
/// `parse` reads holds its ink, and a region of the same size elsewhere is
/// blank: renders and line rectangles share one frame (macOS only).
#[cfg(target_os = "macos")]
#[tokio::test]
async fn the_real_helper_renders_where_parse_reads_on_turned_pages() {
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
    for rotation in [0, 90, 180, 270] {
        let pdf = root.join(format!("tests/fixtures/pdfkit/render/rot{rotation}.pdf"));
        let layer = e.parse(CONTENT, "application/pdf", &pdf).await.unwrap();
        let block = &layer.blocks[0];
        assert_eq!(block.text, "MARK", "{rotation}");
        let [x0, y0, x1, y1] = block.lines[0].rect;
        // The page as displayed: turned a quarter, it lies on its side.
        let (w, h) = if rotation % 180 == 0 {
            (612.0, 792.0)
        } else {
            (792.0, 612.0)
        };
        let blank = [
            w - (x1 - x0) - 10.0,
            h - (y1 - y0) - 10.0,
            w - 10.0,
            h - 10.0,
        ];
        let mut sizes = Vec::new();
        for region in [[x0, y0, x1, y1], blank] {
            let ask = RenderAsk {
                page: 1,
                scale: 2.0,
                region: Some(region),
                max_bytes: 1 << 20,
            };
            let png = e.render("application/pdf", &pdf, ask).await.unwrap().png;
            sizes.push(png.len());
        }
        assert!(
            sizes[0] > 2 * sizes[1],
            "{rotation}: ink {} vs blank {}",
            sizes[0],
            sizes[1]
        );
        // A region starting left of or above the page is refused, not moved.
        let ask = RenderAsk {
            page: 1,
            scale: 1.0,
            region: Some([-20.0, y0, x1, y1]),
            max_bytes: 1 << 20,
        };
        assert_eq!(
            e.render("application/pdf", &pdf, ask).await.err(),
            Some(RenderError::OffPage)
        );
    }
}
