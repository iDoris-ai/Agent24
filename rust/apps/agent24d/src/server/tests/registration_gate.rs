    fn router_with_comm(state: AppState) -> Router {
        let modules = Router::new().nest(
            "/api/v1/comm",
            agent24_comm::router(agent24_comm::CommState::unconfigured("coverage"))
                .route("/install", axum::routing::post(attached_probe)),
        );
        build_router_with_modules(state, modules)
    }

    fn function_span(src: &str, signature: &str) -> std::ops::Range<usize> {
        let start = src
            .find(signature)
            .unwrap_or_else(|| panic!("missing {signature}"));
        let end = src[start..]
            .find("\n}\n")
            .unwrap_or_else(|| panic!("unclosed {signature}"));
        start..start + end
    }

    fn ident_boundary_before(src: &str, at: usize) -> bool {
        match src[..at].chars().next_back() {
            Some(ch) => !ch.is_ascii_alphanumeric() && ch != '_',
            None => true,
        }
    }

    fn line_comment_end(src: &str, at: usize) -> usize {
        match src[at..].find('\n') {
            Some(rel) => at + rel + 1,
            None => src.len(),
        }
    }

    fn block_comment_end(src: &str, at: usize) -> usize {
        let mut i = at + 2;
        let mut depth = 1;
        while i < src.len() && depth > 0 {
            if src[i..].starts_with("/*") {
                depth += 1;
                i += 2;
            } else if src[i..].starts_with("*/") {
                depth -= 1;
                i += 2;
            } else {
                i += src[i..].chars().next().unwrap().len_utf8();
            }
        }
        i
    }

    /// `r"..."`, `r#"..."#`, and the `b`/`c` prefixes. `at` may sit on the prefix.
    fn raw_string_end(src: &str, at: usize) -> Option<usize> {
        let rest = &src[at..];
        let mut rel = 0;
        if rest.starts_with('b') || rest.starts_with('c') {
            if !ident_boundary_before(src, at) {
                return None;
            }
            rel = 1;
        } else if !rest.starts_with('r') {
            return None;
        }
        if !rest[rel..].starts_with('r') {
            return None;
        }
        if rel == 0 && !ident_boundary_before(src, at) {
            return None;
        }
        let bytes = src.as_bytes();
        let mut i = at + rel + 1;
        let mut hashes = 0;
        while bytes.get(i) == Some(&b'#') {
            hashes += 1;
            i += 1;
        }
        if bytes.get(i) != Some(&b'"') {
            return None;
        }
        i += 1;
        let mut close = String::from("\"");
        close.extend(std::iter::repeat_n('#', hashes));
        while i < src.len() {
            if src[i..].starts_with(&close) {
                return Some(i + close.len());
            }
            i += src[i..].chars().next().unwrap().len_utf8();
        }
        Some(src.len())
    }

    fn cooked_string_end(src: &str, at: usize) -> usize {
        let mut i = at + 1;
        let mut escaped = false;
        while i < src.len() {
            let ch = src[i..].chars().next().unwrap();
            i += ch.len_utf8();
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => return i,
                _ => {}
            }
        }
        src.len()
    }

    fn char_literal_end(src: &str, at: usize) -> Option<usize> {
        let rest = &src[at..];
        if !rest.starts_with('\'') {
            return None;
        }
        let mut i = at + 1;
        if i >= src.len() {
            return None;
        }
        if src[i..].starts_with('\\') {
            i += 1;
            if src[i..].starts_with("u{") {
                let close = src[i..].find('}')?;
                i += close + 1;
            } else {
                i += src[i..].chars().next()?.len_utf8();
            }
        } else {
            let ch = src[i..].chars().next()?;
            if ch.is_ascii_alphabetic() || ch == '_' {
                return None;
            }
            i += ch.len_utf8();
        }
        if src[i..].starts_with('\'') {
            Some(i + 1)
        } else {
            None
        }
    }

    /// End index when `src[at..]` opens a comment, string, or char literal.
    fn non_code_end(src: &str, at: usize) -> Option<usize> {
        let rest = &src[at..];
        if rest.starts_with("//") {
            return Some(line_comment_end(src, at));
        }
        if rest.starts_with("/*") {
            return Some(block_comment_end(src, at));
        }
        if (rest.starts_with('b') || rest.starts_with('c') || rest.starts_with('r'))
            && let Some(end) = raw_string_end(src, at)
        {
            return Some(end);
        }
        if rest.starts_with('"') || rest.starts_with("b\"") || rest.starts_with("c\"") {
            let quote = if rest.starts_with('"') { at } else { at + 1 };
            return Some(cooked_string_end(src, quote));
        }
        if rest.starts_with('\'') {
            return char_literal_end(src, at);
        }
        None
    }

    fn skip_trivia(src: &str) -> &str {
        let mut i = 0;
        loop {
            let rest = &src[i..];
            let trimmed = rest.trim_start();
            if trimmed.len() != rest.len() {
                i += rest.len() - trimmed.len();
                continue;
            }
            if (rest.starts_with("//") || rest.starts_with("/*"))
                && let Some(end) = non_code_end(src, i)
            {
                i = end;
                continue;
            }
            return &src[i..];
        }
    }

    fn route_interior(src: &str) -> &str {
        let mut depth = 1;
        let mut i = 0;
        while i < src.len() {
            if let Some(end) = non_code_end(src, i) {
                i = end;
                continue;
            }
            let ch = src[i..].chars().next().unwrap();
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return &src[..i];
                    }
                }
                _ => {}
            }
            i += ch.len_utf8();
        }
        panic!("unbalanced .route(");
    }

    fn rust_string(src: &str) -> Option<(&str, &str)> {
        let src = skip_trivia(src);
        let mut chars = src.char_indices();
        if chars.next()?.1 != '"' {
            return None;
        }
        let mut escaped = false;
        for (index, ch) in chars {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => return Some((&src[1..index], &src[index + 1..])),
                _ => {}
            }
        }
        None
    }

    fn method_names(expr: &str) -> Vec<&'static str> {
        // Every axum method registrar is recognised, so an extra registration
        // (`.options(..)`, `any(..)`, `on(..)`) shows up as a (method, path) pair
        // the catalog does not list instead of being skipped.
        const NAMES: &[&str] = &[
            "get", "post", "put", "patch", "delete", "head", "options", "trace", "connect", "any",
            "on",
        ];
        let mut found = Vec::new();
        let mut index = 0;
        let mut boundary = true;
        while index < expr.len() {
            if let Some(end) = non_code_end(expr, index) {
                index = end;
                boundary = true;
                continue;
            }
            let ch = expr[index..].chars().next().unwrap();
            if boundary {
                for name in NAMES {
                    if !expr[index..].starts_with(name) {
                        continue;
                    }
                    let end = index + name.len();
                    let next = expr[end..].chars().next();
                    let ended = next.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
                    if ended && expr[end..].starts_with('(') {
                        found.push(*name);
                    }
                    break;
                }
            }
            boundary = !ch.is_ascii_alphanumeric() && ch != '_';
            index += ch.len_utf8();
        }
        found
    }

    /// Every `.route(` in `signature`. Comments are not calls. Unparseable calls,
    /// calls with no method, and duplicate method/template pairs fail closed.
    /// Other axum registrars are policed by [`registration_violations`].
    fn parsed_routes(src: &str, signature: &str, prefix: &str) -> BTreeSet<(String, String)> {
        let span = function_span(src, signature);
        let body = &src[span];
        let mut routes = BTreeSet::new();
        for call in code_calls(body) {
            if call.form != "route" {
                continue;
            }
            let interior = route_interior(&body[call.at + ".route(".len()..]);
            let (path, after) = rust_string(interior)
                .unwrap_or_else(|| panic!("unparseable .route( near {signature}"));
            let methods = method_names(after);
            assert!(!methods.is_empty(), "no method on {path}");
            for name in methods {
                let template = if path.starts_with("/api/v1") {
                    path.to_owned()
                } else {
                    format!("{prefix}{path}")
                };
                assert!(
                    routes.insert((name.to_ascii_uppercase(), template.clone())),
                    "duplicate {name} {template}"
                );
            }
        }
        assert!(!routes.is_empty(), "{signature} registered no routes");
        routes
    }

    struct Registration {
        form: &'static str,
        at: usize,
    }

    /// Longest form first, so `.route_service(` is not read as `.route(`.
    const REGISTRATION_FORMS: &[&str] = &[
        "fallback_service",
        "route_service",
        "nest_service",
        "route_layer",
        "fallback",
        "route",
        "nest",
        "merge",
    ];

    fn registration_at(src: &str, at: usize) -> Option<&'static str> {
        let rest = &src[at..];
        if !rest.starts_with('.') {
            return None;
        }
        let rest = &rest[1..];
        REGISTRATION_FORMS
            .iter()
            .copied()
            .find(|form| rest.starts_with(form) && rest[form.len()..].starts_with('('))
    }

    fn code_calls(src: &str) -> Vec<Registration> {
        let mut calls = Vec::new();
        let mut i = 0;
        while i < src.len() {
            if let Some(end) = non_code_end(src, i) {
                i = end;
                continue;
            }
            if let Some(form) = registration_at(src, i) {
                calls.push(Registration { form, at: i });
            }
            i += src[i..].chars().next().unwrap().len_utf8();
        }
        calls
    }

    fn skip_ws_idx(src: &str, mut i: usize) -> usize {
        while let Some(ch) = src[i..].chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            i += ch.len_utf8();
        }
        i
    }

    fn skip_bracket_group(src: &str, at: usize) -> usize {
        let start = if src[at..].starts_with('#') {
            at + 1
        } else {
            at
        };
        let mut i = start + 1;
        let mut depth = 1;
        while i < src.len() && depth > 0 {
            if let Some(end) = non_code_end(src, i) {
                i = end;
                continue;
            }
            match src[i..].chars().next().unwrap() {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
            i += src[i..].chars().next().unwrap().len_utf8();
        }
        i
    }

    fn skip_paren_group(src: &str, at: usize) -> usize {
        let mut i = at + 1;
        let mut depth = 1;
        while i < src.len() && depth > 0 {
            if let Some(end) = non_code_end(src, i) {
                i = end;
                continue;
            }
            match src[i..].chars().next().unwrap() {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            i += src[i..].chars().next().unwrap().len_utf8();
        }
        i
    }

    fn skip_visibility(src: &str, i: usize) -> usize {
        let i = skip_ws_idx(src, i);
        if !src[i..].starts_with("pub") {
            return i;
        }
        let after = i + 3;
        let next = src[after..].chars().next();
        if next.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
            return i;
        }
        let i = skip_ws_idx(src, after);
        if src[i..].starts_with('(') {
            skip_paren_group(src, i)
        } else {
            i
        }
    }

    /// One Rust item: through the matching `}` of a body, or through `;`.
    fn skip_item_body(src: &str, mut i: usize) -> usize {
        let mut brace = 0i32;
        let mut paren = 0i32;
        let mut bracket = 0i32;
        let mut seen_brace = false;
        while i < src.len() {
            if let Some(end) = non_code_end(src, i) {
                i = end;
                continue;
            }
            let ch = src[i..].chars().next().unwrap();
            match ch {
                '{' => {
                    brace += 1;
                    seen_brace = true;
                }
                '}' => {
                    brace -= 1;
                    if seen_brace && brace == 0 && paren == 0 && bracket == 0 {
                        return i + ch.len_utf8();
                    }
                }
                '(' => paren += 1,
                ')' => paren -= 1,
                '[' => bracket += 1,
                ']' => bracket -= 1,
                ';' if brace == 0 && paren == 0 && bracket == 0 => return i + ch.len_utf8(),
                _ => {}
            }
            i += ch.len_utf8();
        }
        i
    }

    fn skip_cfg_test_item(src: &str, i: usize) -> usize {
        let mut i = i + "#[cfg(test)]".len();
        loop {
            i = skip_ws_idx(src, i);
            if (src[i..].starts_with("//") || src[i..].starts_with("/*"))
                && let Some(end) = non_code_end(src, i)
            {
                i = end;
                continue;
            }
            if src[i..].starts_with("#[") {
                i = skip_bracket_group(src, i);
                continue;
            }
            break;
        }
        skip_item_body(src, skip_visibility(src, i))
    }

    fn strip_cfg_test(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let mut i = 0;
        while i < src.len() {
            if let Some(end) = non_code_end(src, i) {
                out.push_str(&src[i..end]);
                i = end;
                continue;
            }
            if src[i..].starts_with("#[cfg(test)]") {
                i = skip_cfg_test_item(src, i);
                continue;
            }
            let ch = src[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }

    /// `at` is a code byte, not inside a comment or string.
    fn is_code_index(src: &str, at: usize) -> bool {
        let mut i = 0;
        while i < at {
            if let Some(end) = non_code_end(src, i) {
                if at < end {
                    return false;
                }
                i = end;
                continue;
            }
            i += src[i..].chars().next().unwrap().len_utf8();
        }
        i == at && non_code_end(src, at).is_none()
    }

    struct AllowedRegistration {
        file: &'static str,
        form: &'static str,
        /// Exact call text in production code. Must occur once and cover one call.
        call: &'static str,
        reason: &'static str,
    }

    /// Axum registrars that are not `.route(` inside the two catalog functions.
    /// `build_router_with_modules` and `agent24_comm::router` are parsed into the
    /// operation catalog; every other registration in agent24d / agent24-comm
    /// production source must be listed here.
    const ALLOWED_REGISTRATIONS: &[AllowedRegistration] = &[
        AllowedRegistration {
            file: "apps/agent24d/src/server.rs",
            form: "fallback",
            call: ".fallback(fallback)",
            reason: "v1 fallback：未登记路径的 kernel 404，不进 Operation 目录",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/server.rs",
            form: "merge",
            call: ".merge(modules)",
            reason: "模块路由 merge 入口：auth layer 之前折入 domain OS",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/server/serve.rs",
            form: "merge",
            call: "module_routes.merge(comm_router)",
            reason: "comm 路由 merge 入口：并入 module_routes，使 /api/v1/comm 走 kernel auth",
        },
        AllowedRegistration {
            file: "crates/agent24-comm/src/router.rs",
            form: "merge",
            call: ".merge(crate::groupchat::routes())",
            reason: "群聊路由并入 comm router；方法清单在 groupchat::routes，由目录扫描覆盖",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/comm_routes.rs",
            form: "nest",
            call: ".nest(\"/api/v1/comm\", agent24_comm::router(state))",
            reason: "comm 前缀挂载点；方法清单在 agent24-comm::router，由目录扫描覆盖",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/comm_routes.rs",
            form: "route_layer",
            call: ".route_layer(middleware::from_fn_with_state(",
            reason: "保留固定路由模板，仅将请求转发到热切换后的通信实例",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/comm_routes.rs",
            form: "nest",
            call: ".nest(\"/api/v1/comm\", routes)",
            reason: "热切换 comm 前缀挂载；runtime_router 的方法清单由目录扫描覆盖",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/docked_router.rs",
            form: "fallback",
            call: ".fallback(not_found)",
            reason: "停靠 router 的 404：未登记路径一律 not_found，不进 Operation 目录",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/domain.rs",
            form: "nest",
            call: "app.nest(namespace, Router::new().fallback(handler.clone()))",
            reason: "模块命名空间 503 的 nest，路径来自 namespace，不是固定 Operation",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/domain.rs",
            form: "fallback",
            call: "Router::new().fallback(handler.clone())",
            reason: "模块命名空间 503 的 fallback，应答该命名空间下全部方法",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/domain.rs",
            form: "route",
            call: ".route(&format!(\"{namespace}/\"), axum::routing::any(handler))",
            reason: "命名空间尾斜杠 503：matchit 不匹配空 segment",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/domain.rs",
            form: "nest",
            call: "app.nest(&namespace, module.routes(ctx))",
            reason: "已挂载模块的动态路由入口，路径来自 manifest",
        },
    ];

    fn production_sources() -> Vec<(String, String)> {
        let rust_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for root in ["apps/agent24d/src", "crates/agent24-comm/src"] {
            collect_rs(&rust_root.join(root), root, &mut files);
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files
    }

    fn collect_rs(dir: &std::path::Path, rel: &str, out: &mut Vec<(String, String)>) {
        let entries =
            std::fs::read_dir(dir).unwrap_or_else(|err| panic!("read {}: {err}", dir.display()));
        for entry in entries {
            let entry = entry.unwrap_or_else(|err| panic!("read {}: {err}", dir.display()));
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "tests" {
                continue;
            }
            let path = entry.path();
            let rel_path = format!("{rel}/{name}");
            if path.is_dir() {
                collect_rs(&path, &rel_path, out);
                continue;
            }
            if !name.ends_with(".rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            out.push((rel_path, strip_cfg_test(&text)));
        }
    }

    fn source_of<'a>(files: &'a [(String, String)], path: &str) -> &'a str {
        files
            .iter()
            .find(|(name, _)| name == path)
            .unwrap_or_else(|| panic!("missing {path}"))
            .1
            .as_str()
    }

    fn preview_at(src: &str, at: usize) -> String {
        let end = src[at..]
            .find('\n')
            .map_or(src.len(), |rel| at + rel)
            .min(at + 160);
        src[at..end].trim().to_owned()
    }

    fn registrar_span(path: &str, src: &str) -> Option<std::ops::Range<usize>> {
        let signature = match path {
            "apps/agent24d/src/server.rs" => "pub fn build_router_with_modules",
            "crates/agent24-comm/src/router.rs" => "pub fn router(",
            "crates/agent24-comm/src/groupchat.rs" => "pub(crate) fn routes(",
            "apps/agent24d/src/comm_routes.rs" => "fn runtime_router(",
            "apps/agent24d/src/docked_router.rs" => "pub fn docked_router",
            _ => return None,
        };
        Some(function_span(src, signature))
    }

    /// Empty means every production axum registration is either a catalog
    /// `.route(` or a live whitelist entry. A missing whitelist call is a
    /// violation, so deleting the real call fails the gate.
    fn registration_violations(files: &[(String, String)]) -> Vec<String> {
        let mut violations = Vec::new();
        let mut ranges: Vec<(&str, std::ops::Range<usize>, &str)> = Vec::new();
        for entry in ALLOWED_REGISTRATIONS {
            let Some(src) = files
                .iter()
                .find(|(path, _)| path == entry.file)
                .map(|(_, text)| text.as_str())
            else {
                violations.push(format!("whitelist file missing: {}", entry.file));
                continue;
            };
            let mut starts = Vec::new();
            let mut search = 0;
            while let Some(rel) = src[search..].find(entry.call) {
                let at = search + rel;
                if is_code_index(src, at) {
                    starts.push(at);
                }
                search = at + entry.call.len();
            }
            if starts.len() != 1 {
                violations.push(format!(
                    "whitelist entry must occur once, found {} in {}: `{}` ({})",
                    starts.len(),
                    entry.file,
                    entry.call,
                    entry.reason
                ));
                continue;
            }
            let start = starts[0];
            let range = start..start + entry.call.len();
            let covered = code_calls(src)
                .iter()
                .filter(|call| call.form == entry.form && range.contains(&call.at))
                .count();
            if covered != 1 {
                violations.push(format!(
                    "whitelist entry does not cover exactly one {} call (covered {covered}) in {}: `{}` ({})",
                    entry.form, entry.file, entry.call, entry.reason
                ));
                continue;
            }
            ranges.push((entry.file, range, entry.form));
        }
        for (path, src) in files {
            let span = registrar_span(path, src);
            for call in code_calls(src) {
                let in_catalog =
                    call.form == "route" && span.as_ref().is_some_and(|s| s.contains(&call.at));
                let listed = ranges.iter().any(|(file, range, form)| {
                    *file == path && *form == call.form && range.contains(&call.at)
                });
                if !in_catalog && !listed {
                    violations.push(format!(
                        "unlisted {} in {path}: {}",
                        call.form,
                        preview_at(src, call.at)
                    ));
                }
            }
        }
        violations
    }

    fn replace_server(files: &[(String, String)], from: &str, to: &str) -> Vec<(String, String)> {
        let mut files = files.to_vec();
        let server = files
            .iter_mut()
            .find(|(path, _)| path == "apps/agent24d/src/server.rs")
            .expect("server.rs");
        let count = server.1.matches(from).count();
        assert_eq!(count, 1, "expected one occurrence of {from}, found {count}");
        server.1 = server.1.replacen(from, to, 1);
        files
    }

    #[test]
    fn a_route_with_a_chinese_comment_parses() {
        let src = "\
pub fn build_router_with_modules() {
    .route(
        /* 中文 */
        \"/api/v1/health\",
        get(/* 中文) */ health).post(health), // 行注释 post(
    )
}
";
        let routes = parsed_routes(src, "pub fn build_router_with_modules", "");
        assert_eq!(
            routes,
            BTreeSet::from([
                ("GET".to_owned(), "/api/v1/health".to_owned()),
                ("POST".to_owned(), "/api/v1/health".to_owned()),
            ])
        );
    }

    #[test]
    fn an_unlisted_route_service_fails_the_registration_gate() {
        let files = replace_server(
            &production_sources(),
            ".route(\"/api/v1/health\", get(health))",
            ".route(\"/api/v1/health\", get(health))\n        .route_service(\"/api/v1/x\", get(health))",
        );
        let violations = registration_violations(&files);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("route_service") && v.contains("/api/v1/x")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_unlisted_nest_fails_the_registration_gate() {
        let files = replace_server(
            &production_sources(),
            ".route(\"/api/v1/health\", get(health))",
            ".nest(\"/api/v1/y\", Router::new())\n        .route(\"/api/v1/health\", get(health))",
        );
        let violations = registration_violations(&files);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("unlisted nest") && v.contains("/api/v1/y")),
            "{violations:?}"
        );
    }

    #[test]
    fn removing_a_whitelisted_merge_fails_the_registration_gate() {
        let files = replace_server(&production_sources(), ".merge(modules)", "");
        let violations = registration_violations(&files);
        assert!(
            violations.iter().any(|v| {
                v.contains(".merge(modules)") && v.contains("模块路由 merge 入口")
            }),
            "{violations:?}"
        );
    }

    #[tokio::test]
    async fn registered_routes_match_the_operation_catalog() {
        let files = production_sources();
        let kernel_src = source_of(&files, "apps/agent24d/src/server.rs");
        let comm_src = source_of(&files, "crates/agent24-comm/src/router.rs");
        assert!(
            !kernel_src.contains("fn registered_routes_match_the_operation_catalog"),
            "cfg(test) module was scanned as production"
        );
        assert_eq!(kernel_src.matches(".merge(modules)").count(), 1);
        let kernel = parsed_routes(kernel_src, "pub fn build_router_with_modules", "");
        let mut comm = parsed_routes(comm_src, "pub fn router(", "/api/v1/comm");
        // `router` merges `groupchat::routes()`; both register under the comm prefix.
        let groupchat_src = source_of(&files, "crates/agent24-comm/src/groupchat.rs");
        comm.extend(parsed_routes(
            groupchat_src,
            "pub(crate) fn routes(",
            "/api/v1/comm",
        ));
        comm.extend(parsed_routes(
            source_of(&files, "apps/agent24d/src/comm_routes.rs"),
            "fn runtime_router(",
            "/api/v1/comm",
        ));
        let violations = registration_violations(&files);
        assert!(
            violations.is_empty(),
            "registration gate:\n{}",
            violations.join("\n")
        );
        assert!(kernel.len() >= 50, "kernel scan saw only {}", kernel.len());
        let comm_paths = comm
            .iter()
            .map(|(_, path)| path.as_str())
            .collect::<BTreeSet<_>>();
        assert!(
            comm_paths.len() >= 16,
            "comm scan saw only {}",
            comm_paths.len()
        );

        let mut registered = kernel.clone();
        registered.extend(comm.iter().cloned());
        // CORE-5f-2: docked-only routes are catalogued but not mounted on the
        // business router, so they join the catalog comparison, not the 405 sweep.
        let docked = parsed_routes(
            source_of(&files, "apps/agent24d/src/docked_router.rs"),
            "pub fn docked_router",
            "",
        );
        let business = registered.clone();
        registered.extend(docked.iter().cloned());
        let catalog = crate::capabilities::policies()
            .iter()
            .map(|policy| (policy.method.to_owned(), policy.template.to_owned()))
            .collect::<BTreeSet<_>>();
        assert_eq!(registered, catalog);

        let empty = BTreeSet::new();
        for (method_name, template) in &registered {
            let decision = crate::capabilities::decide(
                &Method::from_bytes(method_name.as_bytes()).unwrap(),
                Some(template),
                template,
                &empty,
            );
            assert!(
                matches!(decision, crate::capabilities::RouteDecision::Mapped(_)),
                "{method_name} {template} fell into the module fallback"
            );
        }

        let (state, host, _) = capability_state().await;
        let router = router_with_comm(state);
        let trace = Method::from_bytes(b"TRACE").unwrap();
        let mut templates = BTreeSet::new();
        for (_, template) in &business {
            templates.insert(template.clone());
        }
        for template in templates {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(trace.clone())
                        .uri(concrete_path(&template))
                        .header(header::AUTHORIZATION, format!("Bearer {host}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{template}"
            );
            let allow = response
                .headers()
                .get(header::ALLOW)
                .unwrap_or_else(|| panic!("no Allow on {template}"))
                .to_str()
                .unwrap();
            let got = allow
                .split(',')
                .map(|part| part.trim().to_owned())
                .collect::<BTreeSet<_>>();
            let mut expect = business
                .iter()
                .filter(|(_, path)| path == &template)
                .map(|(method_name, _)| method_name.clone())
                .collect::<BTreeSet<_>>();
            if expect.contains("GET") {
                expect.insert("HEAD".to_owned());
            }
            assert_eq!(got, expect, "{template} Allow: {allow}");
        }
    }

    #[tokio::test]
    async fn attached_routes_are_host_only_in_capability_mode() {
        let (state, host, creative) = capability_state().await;
        let router = Router::new()
            .route("/api/v1/attached", get(attached_probe).post(attached_probe))
            .route(
                "/api/v1/attached/{name}",
                axum::routing::patch(attached_probe).delete(attached_probe),
            )
            .layer(middleware::from_fn_with_state(state, auth));

        for (method, path) in [
            (Method::GET, "/api/v1/attached"),
            (Method::POST, "/api/v1/attached"),
            (Method::PATCH, "/api/v1/attached/agentear"),
            (Method::DELETE, "/api/v1/attached/agentear"),
        ] {
            let unauthenticated = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                unauthenticated.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
            assert_eq!(
                body_json(unauthenticated).await["error"]["code"],
                "unauthorized"
            );

            let creative_denied = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(path)
                        .header("Authorization", format!("Bearer {creative}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                creative_denied.status(),
                StatusCode::FORBIDDEN,
                "{method} {path}"
            );
            assert_eq!(
                body_json(creative_denied).await["error"]["code"],
                "forbidden"
            );

            let host_allowed = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(path)
                        .header("Authorization", format!("Bearer {host}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                host_allowed.status(),
                StatusCode::NO_CONTENT,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn host_can_mint_and_idempotently_revoke_creative_authority() {
        let (state, host, existing_creative) = capability_state().await;
        let router = build_router(state);

        let creative_mint_attempt = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/capabilities/creative")
                    .header("Authorization", format!("Bearer {existing_creative}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(creative_mint_attempt.status(), StatusCode::FORBIDDEN);

        let minted = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/capabilities/creative")
                    .header("Authorization", format!("Bearer {host}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "workspace_id": "workspace-new",
                            "creative_attachment_id": "attachment-new",
                            "creative_principal_id": "principal-new",
                            "sidecar_generation": "sidecar-new",
                            "ttl_seconds": 60,
                            "allowed_operations": ["models.read"]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(minted.status(), StatusCode::CREATED);
        let minted = body_json(minted).await;
        let id = minted["capability_id"].as_str().unwrap();
        let token = minted["token"].as_str().unwrap();

        for _ in 0..2 {
            let revoked = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/v1/capabilities/{id}/revoke"))
                        .header("Authorization", format!("Bearer {host}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(revoked.status(), StatusCode::OK);
            assert_eq!(body_json(revoked).await["state"], "revoked");
        }

        let rejected = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    }

