//! X11-2h acceptance ① and ②. Reads the worktree; does not start the daemon.
//!
//! Scan method (shared by both checks):
//! - Root is this package's manifest, three levels up (the repo).
//! - Only `.ts` and `.rs`. Skip directories named `node_modules`, `target`,
//!   `dist`, or `.git` (by name, before following a symlink).
//! - Skip tests: `*.test.ts`, `*.spec.ts`, `*_test.rs`, `*_tests.rs`, and any
//!   path component `tests` or `__tests__`.
//! - Strip `//` and `/* */` with a string-aware scan, so a comment that names
//!   the route does not count and a string literal does. Rust raw strings and
//!   char literals stay strings; a `'lifetime` is not a string. `.ts` treats
//!   `'`, `"`, and `` ` `` as strings.
//! - Then drop `#[cfg(test)]` items (the following `mod` / `fn` / `impl`,
//!   or a `use` through `;`, including stacked attributes). An inline test
//!   module is not production.
//!
//! A create POST is the exact path `/api/v1/runs` whose next byte does not
//! continue the path (`/`, `{`, `$`, or an identifier byte), and whose nearest
//! method token in the previous 400 bytes is a POST (`.post(`, `POST`, or
//! `Method::POST`) rather than GET / PUT / PATCH / DELETE. The 400-byte window
//! is why a comment that was already stripped cannot trip the check, and why
//! an earlier POST on a different route does not paint a later GET.
//!
//! Create-caller whitelist (the route registered in `agent24d` is the callee
//! and that whole tree is not scanned):
//! - `apps/desktop/` — the product UI starts a user run.
//! - `rust/apps/agent24-cli/` — the user CLI. The create call is `acp.rs`;
//!   cancel and GET are different paths. A production string in this tree that
//!   still says POST of the exact create path stays inside the whitelist.
//! - `rust/crates/agent24-mcp/` — MCP clients start a run through the daemon.
//!
//! `packages/` (including wechat-bridge) and `agent24-comm` are not callers.
//! wechat-bridge left this path in X11-2g. comm stays zero-run: its dependency
//! allowlist forbids an HTTP client (`comm_dependency_allowlist.rs`).
//!
//! `start_run*` is scanned only under `agent24d/src`, after the same comment
//! and `#[cfg(test)]` strip. Longer names are distinct because the match
//! requires `(` immediately after optional whitespace, so `start_run_then`
//! is not also `start_run`.

const CREATE_PATH: &str = "/api/v1/runs";
const METHOD_WINDOW: usize = 400;

#[test]
fn packages_and_comm_do_not_post_runs_and_create_callers_stay_on_the_whitelist() {
    let root = repo_root();
    let forbidden = create_posts_under(&root, &["packages", "rust/crates/agent24-comm"]);
    assert!(
        forbidden.is_empty(),
        "packages or agent24-comm POST {CREATE_PATH}: {forbidden:?}"
    );

    let mut hits = create_posts_under(&root, &["apps", "packages", "rust/apps", "rust/crates"]);
    hits.retain(|path| !path.starts_with("rust/apps/agent24d/"));
    let stray: Vec<_> = hits
        .iter()
        .filter(|path| !create_caller_allowed(path))
        .cloned()
        .collect();
    assert!(
        stray.is_empty(),
        "POST {CREATE_PATH} outside desktop / CLI / MCP: {stray:?}\nall: {hits:?}"
    );
    assert!(
        hits.iter()
            .any(|path| path == "rust/apps/agent24-cli/src/acp.rs"),
        "scanner missed acp.rs: {hits:?}"
    );
    assert!(
        hits.iter()
            .any(|path| path == "rust/crates/agent24-mcp/src/server.rs"),
        "scanner missed agent24-mcp: {hits:?}"
    );
    assert!(
        hits.iter().any(|path| path.starts_with("apps/desktop/")),
        "scanner missed desktop: {hits:?}"
    );
}

#[test]
fn start_run_inbound_is_only_called_from_the_inbound_service() {
    let src = repo_root().join("rust/apps/agent24d/src");
    let mut calls = Vec::new();
    let mut files = Vec::new();
    walk(&src, &mut files);
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let stripped = strip_cfg_test(&strip_comments(&text, Lang::Rust));
        let rel = path
            .strip_prefix(&src)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        for name in start_run_calls(&stripped) {
            calls.push((rel.clone(), name));
        }
    }
    calls.sort();
    let inbound: Vec<_> = calls
        .iter()
        .filter(|(_, name)| name == "start_run_inbound")
        .map(|(file, _)| file.as_str())
        .collect();
    let then: Vec<_> = calls
        .iter()
        .filter(|(_, name)| name == "start_run_then")
        .map(|(file, _)| file.as_str())
        .collect();
    let scheduled: Vec<_> = calls
        .iter()
        .filter(|(_, name)| name == "start_run_with_schedule")
        .map(|(file, _)| file.as_str())
        .collect();
    let bare: Vec<_> = calls
        .iter()
        .filter(|(_, name)| name == "start_run")
        .map(|(file, _)| file.as_str())
        .collect();
    assert_eq!(
        inbound,
        vec!["inbound/service.rs"],
        "start_run_inbound call sites: {calls:?}"
    );
    assert_eq!(
        then,
        vec!["runs.rs"],
        "start_run_then call sites: {calls:?}"
    );
    assert_eq!(
        scheduled,
        vec!["server.rs"],
        "start_run_with_schedule call sites: {calls:?}"
    );
    assert!(
        bare.is_empty(),
        "production start_run call sites: {calls:?}"
    );
    assert_eq!(calls.len(), 3, "unexpected start_run* calls: {calls:?}");
}

#[test]
fn scan_ignores_comments_tests_and_non_create_paths() {
    let src = r#"
        // POST /api/v1/runs
        /* POST /api/v1/runs */
        r('GET', '/api/v1/runs');
        r('POST', '/api/v1/runs/{id}/cancel');
        r('POST', `/api/v1/runs/${id}/cancel`);
        r('POST', '/api/v1/runs');
        let _ = "POST /api/v1/runs";
    "#;
    let hits = create_post_count(&strip_comments(src, Lang::TypeScript));
    assert_eq!(hits, 2, "comment, GET, and subpaths must not count");

    let rust_src = r#"
        fn prod() { runs.start_run_inbound(x); runs.start_run_then(y); }
        #[cfg(test)]
        mod tests {
            fn hidden() { runs.start_run(z); }
            fn also() { let _ = "POST /api/v1/runs"; }
        }
        #[cfg(test)]
        use crate::ignored;
    "#;
    let stripped = strip_cfg_test(&strip_comments(rust_src, Lang::Rust));
    assert_eq!(
        start_run_calls(&stripped),
        vec!["start_run_inbound".to_owned(), "start_run_then".to_owned()]
    );
    assert_eq!(create_post_count(&stripped), 0);
}

fn create_caller_allowed(path: &str) -> bool {
    path.starts_with("apps/desktop/")
        || path.starts_with("rust/apps/agent24-cli/")
        || path.starts_with("rust/crates/agent24-mcp/")
}

fn create_posts_under(root: &std::path::Path, rels: &[&str]) -> Vec<String> {
    let mut hits = Vec::new();
    for rel in rels {
        let dir = root.join(rel);
        let mut files = Vec::new();
        walk(&dir, &mut files);
        for path in files {
            let lang = match path.extension().and_then(|ext| ext.to_str()) {
                Some("ts") => Lang::TypeScript,
                Some("rs") => Lang::Rust,
                _ => continue,
            };
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let stripped = strip_cfg_test(&strip_comments(&text, lang));
            if create_post_count(&stripped) == 0 {
                continue;
            }
            let rel_path = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            hits.push(rel_path);
        }
    }
    hits.sort();
    hits
}

fn create_post_count(src: &str) -> usize {
    let bytes = src.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i + CREATE_PATH.len() <= bytes.len() {
        if bytes[i..].starts_with(CREATE_PATH.as_bytes()) && !continues_path(bytes, i) {
            let from = i.saturating_sub(METHOD_WINDOW);
            if nearest_is_post(&src[from..i]) {
                count += 1;
            }
            i += CREATE_PATH.len();
            continue;
        }
        i += 1;
    }
    count
}

fn continues_path(bytes: &[u8], path_at: usize) -> bool {
    let next = path_at + CREATE_PATH.len();
    if next >= bytes.len() {
        return false;
    }
    let byte = bytes[next];
    byte == b'/' || byte == b'{' || byte == b'$' || byte.is_ascii_alphanumeric() || byte == b'_'
}

fn nearest_is_post(before: &str) -> bool {
    let bytes = before.as_bytes();
    let mut last_post: Option<bool> = None;
    let mut i = 0;
    while i < bytes.len() {
        if let Some(post) = method_at(bytes, i) {
            last_post = Some(post);
        }
        i += 1;
    }
    last_post == Some(true)
}

fn method_at(bytes: &[u8], i: usize) -> Option<bool> {
    let rest = &bytes[i..];
    if rest.starts_with(b".post(") {
        return Some(true);
    }
    if rest.starts_with(b".get(")
        || rest.starts_with(b".put(")
        || rest.starts_with(b".patch(")
        || rest.starts_with(b".delete(")
    {
        return Some(false);
    }
    for (word, post) in [
        ("POST", true),
        ("GET", false),
        ("PUT", false),
        ("PATCH", false),
        ("DELETE", false),
    ] {
        if rest.starts_with(word.as_bytes()) && word_boundary(bytes, i, word.len()) {
            return Some(post);
        }
    }
    None
}

fn word_boundary(bytes: &[u8], at: usize, len: usize) -> bool {
    let before_ok = at == 0 || !is_ident(bytes[at - 1]);
    let after = at + len;
    let after_ok = after >= bytes.len() || !is_ident(bytes[after]);
    before_ok && after_ok
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn start_run_calls(src: &str) -> Vec<String> {
    const NAMES: [&str; 4] = [
        "start_run_with_schedule",
        "start_run_inbound",
        "start_run_then",
        "start_run",
    ];
    let bytes = src.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(len) = rust_string_len(&src[i..]) {
            i += len;
            continue;
        }
        let rest = &src[i..];
        let mut matched = false;
        for name in NAMES {
            if rest.starts_with(name)
                && (i == 0 || !is_ident(bytes[i - 1]))
                && call_paren(rest, name.len())
            {
                found.push(name.to_owned());
                matched = true;
                break;
            }
        }
        if matched {
            i += 1;
            continue;
        }
        i += src[i..].chars().next().map_or(1, char::len_utf8);
    }
    found
}

fn call_paren(rest: &str, name_len: usize) -> bool {
    rest[name_len..].trim_start().starts_with('(')
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    TypeScript,
}

fn strip_comments(src: &str, lang: Lang) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if lang == Lang::Rust {
            if let Some(len) = rust_string_len(&src[i..]) {
                out.push_str(&src[i..i + len]);
                i += len;
                continue;
            }
        } else if let Some(len) = ts_string_len(&src[i..]) {
            out.push_str(&src[i..i + len]);
            i += len;
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = if i + 1 < bytes.len() {
                i + 2
            } else {
                bytes.len()
            };
            out.push(' ');
            continue;
        }
        let Some(ch) = src[i..].chars().next() else {
            break;
        };
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn strip_cfg_test(src: &str) -> String {
    let mut out = String::new();
    let mut rest = src;
    while let Some(at) = find_cfg_test(rest) {
        out.push_str(&rest[..at]);
        let after = &rest[at + "#[cfg(test)]".len()..];
        let mut skipped = skip_cfg_item(after);
        if skipped == 0 {
            skipped = 1;
        }
        rest = &after[skipped.min(after.len())..];
    }
    out.push_str(rest);
    out
}

fn find_cfg_test(src: &str) -> Option<usize> {
    let needle = "#[cfg(test)]";
    let mut i = 0;
    while i + needle.len() <= src.len() {
        if let Some(len) = rust_string_len(&src[i..]) {
            i += len;
            continue;
        }
        if src[i..].starts_with(needle) {
            return Some(i);
        }
        i += src[i..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn skip_cfg_item(src: &str) -> usize {
    let bytes = src.as_bytes();
    let mut i = 0;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if src[i..].starts_with("#[") {
            i += skip_attribute(&src[i..]);
            continue;
        }
        break;
    }
    let tail = &src[i..];
    let Some(rel) = find_brace_or_semi(tail) else {
        return src.len();
    };
    if tail.as_bytes()[rel] == b';' {
        return i + rel + 1;
    }
    i + brace_end(tail, rel)
}

fn skip_attribute(src: &str) -> usize {
    let bytes = src.as_bytes();
    let mut depth = 0;
    let mut i = 0;
    while i < bytes.len() {
        if let Some(len) = rust_string_len(&src[i..]) {
            i += len;
            continue;
        }
        if bytes[i] == b'[' {
            depth += 1;
        } else if bytes[i] == b']' {
            depth -= 1;
            i += 1;
            if depth == 0 {
                return i;
            }
            continue;
        }
        i += src[i..].chars().next().map_or(1, char::len_utf8);
    }
    src.len()
}

fn find_brace_or_semi(src: &str) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(len) = rust_string_len(&src[i..]) {
            i += len;
            continue;
        }
        if bytes[i] == b'{' || bytes[i] == b';' {
            return Some(i);
        }
        i += src[i..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn brace_end(src: &str, open: usize) -> usize {
    let bytes = src.as_bytes();
    let mut depth = 0;
    let mut i = open;
    while i < bytes.len() {
        if let Some(len) = rust_string_len(&src[i..]) {
            i += len;
            continue;
        }
        if bytes[i] == b'{' {
            depth += 1;
        } else if bytes[i] == b'}' {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += src[i..].chars().next().map_or(1, char::len_utf8);
    }
    src.len()
}

fn rust_string_len(src: &str) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.first().copied() == Some(b'\'') {
        return char_literal_len(src);
    }
    let mut i = 0;
    if matches!(bytes.first().copied(), Some(b'b' | b'c')) && bytes.len() > 1 {
        i = 1;
    }
    if bytes.get(i) == Some(&b'r') {
        return raw_string_len(src, i);
    }
    if bytes.first() == Some(&b'"') {
        return Some(quoted_string_len(src, 1, b'"'));
    }
    None
}

fn raw_string_len(src: &str, r_at: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut i = r_at + 1;
    let mut hashes = 0;
    while bytes.get(i) == Some(&b'#') {
        hashes += 1;
        i += 1;
    }
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    let closer = format!("\"{}", "#".repeat(hashes));
    let rest = &src[i..];
    let end = rest.find(&closer)?;
    Some(i + end + closer.len())
}

fn char_literal_len(src: &str) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.first() != Some(&b'\'') {
        return None;
    }
    if bytes.get(1) == Some(&b'\\') {
        let i = if bytes.get(2) == Some(&b'u') && bytes.get(3) == Some(&b'{') {
            let close = src[4..].find('}')?;
            4 + close + 1
        } else {
            let ch = src[2..].chars().next()?;
            2 + ch.len_utf8()
        };
        if bytes.get(i) == Some(&b'\'') {
            return Some(i + 1);
        }
        return None;
    }
    let ch = src[1..].chars().next()?;
    if ch == '\'' {
        return None;
    }
    let after = 1 + ch.len_utf8();
    if bytes.get(after) == Some(&b'\'') {
        Some(after + 1)
    } else {
        None
    }
}

fn ts_string_len(src: &str) -> Option<usize> {
    let quote = *src.as_bytes().first()?;
    if quote == b'\'' || quote == b'"' || quote == b'`' {
        Some(quoted_string_len(src, 1, quote))
    } else {
        None
    }
}

fn quoted_string_len(src: &str, start: usize, quote: u8) -> usize {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 1;
            if i < bytes.len() {
                i += src[i..].chars().next().map_or(1, char::len_utf8);
            }
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += src[i..].chars().next().map_or(1, char::len_utf8);
    }
    src.len()
}

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(name.as_ref(), "node_modules" | "target" | "dist" | ".git") {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
            continue;
        }
        if is_scanned_source(&path) {
            out.push(path);
        }
    }
}

fn is_scanned_source(path: &std::path::Path) -> bool {
    let ext = path.extension().and_then(|ext| ext.to_str());
    if ext != Some("ts") && ext != Some("rs") {
        return false;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.ends_with(".test.ts")
        || name.ends_with(".spec.ts")
        || name.ends_with("_test.rs")
        || name.ends_with("_tests.rs")
    {
        return false;
    }
    !path
        .components()
        .any(|component| matches!(component.as_os_str().to_str(), Some("tests" | "__tests__")))
}

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
