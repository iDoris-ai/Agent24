#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use super::*;

fn skill(extra: &str, body: &str) -> String {
    format!(
        "---\nname: weekly-report\nversion: 1.2.0\ndescription: {{ zh: 周报 }}\nrequires: [documents]\nevals: evals/check.jsonl\n{extra}---\n{body}"
    )
}

fn parsed(extra: &str, body: &str) -> SkillManifest {
    parse_skill_markdown(&skill(extra, body)).unwrap()
}

fn code_of(markdown: &str) -> &'static str {
    parse_skill_markdown(markdown).unwrap_err().code()
}

#[test]
fn sha256_of_abc_is_the_standard_vector() {
    assert_eq!(
        sha256_hex("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn missing_required_fields_fail() {
    let base = skill("", "hello");
    for key in ["name", "version", "description", "requires", "evals"] {
        let line = match key {
            "name" => "name: weekly-report\n",
            "version" => "version: 1.2.0\n",
            "description" => "description: { zh: 周报 }\n",
            "requires" => "requires: [documents]\n",
            "evals" => "evals: evals/check.jsonl\n",
            _ => unreachable!(),
        };
        assert_eq!(
            code_of(&base.replace(line, "")),
            "skill_manifest_invalid",
            "{key}"
        );
    }
}

#[test]
fn name_uses_the_capability_id_charset() {
    let ok = format!("a{}", "b".repeat(31));
    assert!(is_capability_id_shape(&ok));
    assert!(parse_skill_markdown(&skill("", "x").replace("weekly-report", &ok)).is_ok());
    for bad in [
        "a",
        &format!("a{}", "b".repeat(32)),
        "-ab",
        "Weekly",
        "has_under",
    ] {
        assert!(!is_capability_id_shape(bad), "{bad}");
        assert_eq!(
            code_of(&skill("", "x").replace("weekly-report", bad)),
            "skill_manifest_invalid",
            "{bad}"
        );
    }
}

#[test]
fn scans_is_a_reserved_skill_id() {
    assert_eq!(
        code_of(&skill("", "x").replace("weekly-report", "scans")),
        "skill_id_reserved"
    );
    let imported = "---\nname: scans\ndescription: 周报\n---\nhello\n";
    assert_eq!(
        parse_imported(imported).unwrap_err().code(),
        "skill_id_reserved"
    );
}

#[test]
fn version_is_semver_metadata_and_is_not_part_of_the_digest() {
    let low = parsed("", "hello");
    let high =
        parse_skill_markdown(&skill("", "hello").replace("version: 1.2.0", "version: 9.9.9"))
            .unwrap();
    assert_eq!(low.version, "1.2.0");
    assert_eq!(high.version, "9.9.9");
    assert_eq!(low.content_digest(), high.content_digest());
    assert_eq!(
        code_of(&skill("", "x").replace("version: 1.2.0", "version: 1.2")),
        "skill_manifest_invalid"
    );
    assert_eq!(
        code_of(&skill("", "x").replace("version: 1.2.0", "version: 01.2.3")),
        "skill_manifest_invalid"
    );
    assert_eq!(
        code_of(&skill("", "x").replace("version: 1.2.0", "version: v1.2.0")),
        "skill_manifest_invalid"
    );
    let pre = skill("", "x").replace("version: 1.2.0", "version: 1.2.3-alpha.1+build.7");
    assert_eq!(
        parse_skill_markdown(&pre).unwrap().version,
        "1.2.3-alpha.1+build.7"
    );
}

#[test]
fn description_newlines_cannot_alias_another_language() {
    let only_zh = "---\nname: weekly-report\nversion: 1.2.0\ndescription:\n  zh: \"a\\nen:b\\nth:c\"\nrequires: []\nevals: evals/check.jsonl\n---\nhello\n";
    let split = "---\nname: weekly-report\nversion: 1.2.0\ndescription:\n  zh: \"a\"\n  en: \"b\\nth:c\\nen:\"\nrequires: []\nevals: evals/check.jsonl\n---\nhello\n";
    let a = parse_skill_markdown(only_zh).unwrap();
    let b = parse_skill_markdown(split).unwrap();
    assert_eq!(a.description.zh.as_deref(), Some("a\nen:b\nth:c"));
    assert!(a.description.en.is_none());
    assert!(a.description.th.is_none());
    assert_eq!(b.description.zh.as_deref(), Some("a"));
    assert_eq!(b.description.en.as_deref(), Some("b\nth:c\nen:"));
    assert!(b.description.th.is_none());
    assert_ne!(a.content_digest(), b.content_digest());
}

#[test]
fn description_edits_change_the_digest_and_unknown_keys_do_not() {
    let base = parsed("", "hello");
    let renamed = parse_skill_markdown(&skill("", "hello").replace("周报", "日报")).unwrap();
    assert_ne!(base.content_digest(), renamed.content_digest());
    let with_tools = parsed("allowed-tools: [Shell]\n", "hello");
    assert_eq!(base.content_digest(), with_tools.content_digest());
    assert_ne!(base.content_digest(), parsed("", "hello!").content_digest());
}

#[test]
fn unknown_frontmatter_is_listed_and_stays_out_of_the_structured_result() {
    let manifest = parsed(
        "allowed-tools: [Shell]\nbase: { source: bundled }\norigin: imported\n",
        "hello",
    );
    assert_eq!(manifest.ineffective, ["allowed-tools", "base", "origin"]);
    assert_eq!(manifest.requires, ["documents"]);
    assert_eq!(manifest.mode, SkillMode::Manual);
    let value = serde_json::to_value(&manifest).unwrap();
    let obj = value.as_object().unwrap();
    let mut keys: Vec<_> = obj.keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "body",
            "description",
            "evals",
            "ineffective",
            "mode",
            "name",
            "requires",
            "version"
        ]
    );
    let text = serde_json::to_string(&manifest).unwrap();
    assert!(!text.contains("Shell"));
    assert!(!text.contains("imported"));
    assert!(!text.contains("bundled"));
    assert!(obj.get("allowed_tools").is_none());
    assert!(obj.get("allowed-tools").is_none());
}

#[test]
fn capabilities_key_is_not_copied_into_requires() {
    let manifest = parsed("capabilities: [model-gateway]\n", "hello");
    assert_eq!(manifest.ineffective, ["capabilities"]);
    assert_eq!(manifest.requires, ["documents"]);
    let text = serde_json::to_string(&manifest).unwrap();
    assert!(!text.contains("model-gateway"));
}

#[test]
fn description_accepts_a_scalar_and_drops_unknown_languages() {
    let scalar = skill("", "hello").replace("description: { zh: 周报 }\n", "description: 周报\n");
    let manifest = parse_skill_markdown(&scalar).unwrap();
    assert_eq!(manifest.description.text.as_deref(), Some("周报"));
    assert!(manifest.description.zh.is_none());

    let extra = skill("", "hello").replace("{ zh: 周报 }", "{ zh: 周报, fr: secret-lang }");
    let manifest = parse_skill_markdown(&extra).unwrap();
    assert_eq!(manifest.description.zh.as_deref(), Some("周报"));
    assert_eq!(manifest.ineffective, ["description.fr"]);
    assert!(
        !serde_json::to_string(&manifest)
            .unwrap()
            .contains("secret-lang")
    );
}

#[test]
fn requires_admits_registry_ids_and_mcp_ids() {
    let manifest = parsed("", "hello").requires;
    assert_eq!(manifest, ["documents"]);
    let many = skill("", "x").replace(
        "requires: [documents]",
        "requires: [model-gateway, nostr-dm, nostr-group, nostr-identity, nostr-relay, documents, mcp-files]",
    );
    let ids = parse_skill_markdown(&many).unwrap().requires;
    assert_eq!(ids.len(), 7);
    assert!(is_mcp_capability_id("mcp-files"));
    assert!(is_mcp_capability_id(&format!("mcp-{}", "a".repeat(28))));
    for bad in [
        "Documents",
        "nostr_dm",
        "not-a-cap",
        "mcp-",
        "mcp-a-",
        &format!("mcp-{}", "a".repeat(29)),
    ] {
        assert!(!is_admitted_capability(bad), "{bad}");
        let markdown =
            skill("", "x").replace("requires: [documents]", &format!("requires: [{bad}]"));
        assert_eq!(code_of(&markdown), "skill_capability_unknown", "{bad}");
    }
    let empty = skill("", "x").replace("requires: [documents]", "requires: []");
    assert!(parse_skill_markdown(&empty).unwrap().requires.is_empty());
    let dup = skill("", "x").replace("requires: [documents]", "requires: [documents, documents]");
    assert_eq!(code_of(&dup), "skill_manifest_invalid");
}

#[test]
fn requires_order_does_not_change_the_digest() {
    let ab = parse_skill_markdown(&skill("", "hello").replace(
        "requires: [documents]",
        "requires: [model-gateway, documents]",
    ))
    .unwrap();
    let ba = parse_skill_markdown(&skill("", "hello").replace(
        "requires: [documents]",
        "requires: [documents, model-gateway]",
    ))
    .unwrap();
    assert_eq!(ab.requires, ["model-gateway", "documents"]);
    assert_eq!(ba.requires, ["documents", "model-gateway"]);
    assert_ne!(ab.requires, ba.requires);
    assert_eq!(ab.content_digest(), ba.content_digest());
}

#[test]
fn registered_ids_match_registry_md() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../docs/capabilities/REGISTRY.md");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut rows = Vec::new();
    let mut in_table = false;
    for line in text.lines() {
        if line.starts_with("| 能力 id") {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        if !line.starts_with('|') {
            break;
        }
        let mut cols = line.split('|').map(str::trim);
        let _leading = cols.next();
        let id = cols.next().unwrap().trim_matches('`');
        if id.starts_with('-') {
            continue;
        }
        let name_zh = cols.next().unwrap().to_string();
        rows.push((id.to_string(), name_zh));
    }
    let expected: Vec<_> = REGISTERED_CAPABILITIES
        .iter()
        .map(|row| (row.id.to_string(), row.name_zh.to_string()))
        .collect();
    assert_eq!(rows, expected);
}

#[test]
fn body_limit_is_32768_utf8_bytes() {
    let head = skill("", "");
    assert!(head.len() < BODY_MAX_BYTES);
    let mut at_limit = head.clone();
    at_limit.push_str(&"a".repeat(BODY_MAX_BYTES - head.len()));
    assert_eq!(at_limit.len(), BODY_MAX_BYTES);
    assert!(parse_skill_markdown(&at_limit).is_ok());
    at_limit.push('a');
    assert_eq!(
        parse_skill_markdown(&at_limit).unwrap_err().code(),
        "skill_body_too_large"
    );
}

fn detail(markdown: &str) -> String {
    parse_skill_markdown(markdown).unwrap_err().to_string()
}

#[test]
fn malformed_manifests_and_evals_are_refused() {
    let front =
        "name: ab\nversion: 1.0.0\ndescription: { zh: 周报 }\nrequires: []\nevals: evals/a.jsonl\n";
    assert!(detail("---\n- item\n---\n").contains("not a map"));
    assert!(detail("hello\n").contains("missing frontmatter"));
    assert!(detail(&format!("---\n{front}")).contains("not closed"));
    assert!(detail(&format!("---\n? [1, 2]\n: x\n{front}---\n")).contains("key is not a string"));
    assert!(
        detail(&skill("", "x").replace("description: { zh: 周报 }", "description: \"\""))
            .contains("description is empty")
    );
    assert!(
        detail(&skill("", "x").replace("{ zh: 周报 }", "{ zh: 1 }"))
            .contains("value is not a string")
    );
    assert!(
        detail(&skill("", "x").replace("{ zh: 周报 }", "{ zh: \"\" }")).contains("value is empty")
    );
    assert!(
        detail(&skill("", "x").replace("{ zh: 周报 }", "{ ? [1] : 周报 }"))
            .contains("key is not a string")
    );
    assert!(
        detail(&skill("", "x").replace("{ zh: 周报 }", "{ fr: 其他 }"))
            .contains("description is empty")
    );
    assert!(
        detail(&skill("", "x").replace("requires: [documents]", "requires: documents"))
            .contains("not a list")
    );
    assert!(
        detail(&skill("", "x").replace("requires: [documents]", "requires: [1]"))
            .contains("not a string")
    );
    assert!(
        check("evals/check.jsonl", "[]\n", "step")
            .unwrap_err()
            .to_string()
            .contains("not an object")
    );
    assert!(
        check(
            "evals/check.jsonl",
            r#"{"input":"x","expect":"step","judge":"shrug"}"#,
            "step"
        )
        .unwrap_err()
        .to_string()
        .contains("judge is not supported")
    );
}

#[test]
fn proactive_is_refused_and_a_missing_mode_is_manual() {
    assert_eq!(parsed("", "hello").mode, SkillMode::Manual);
    assert_eq!(
        code_of(&skill("mode: proactive\n", "hello")),
        "skill_proactive_unsupported"
    );
    assert_eq!(
        code_of("---\nmode: proactive\n---\n"),
        "skill_proactive_unsupported"
    );
    assert_eq!(
        code_of(&skill("mode: observe\n", "hello")),
        "skill_manifest_invalid"
    );
    assert_eq!(
        code_of(&skill("mode: auto\n", "hello")),
        "skill_manifest_invalid"
    );
    assert_eq!(code_of("hello\n"), "skill_manifest_invalid");
}

#[test]
fn crlf_frontmatter_round_trips_the_body() {
    let markdown = "---\r\nname: weekly-report\r\nversion: 1.2.0\r\ndescription: { zh: 周报 }\r\nrequires: []\r\nevals: evals/check.jsonl\r\n---\r\nline\r\n";
    let manifest = parse_skill_markdown(markdown).unwrap();
    assert_eq!(manifest.body, "line\r\n");
    assert!(manifest.requires.is_empty());
}

#[test]
fn render_skill_segment_is_the_exact_header_plus_body() {
    let manifest = parsed("", "step one");
    let digest = manifest.content_digest();
    let segment = render_skill_segment(&manifest.body, SkillOrigin::User, &digest);
    assert_eq!(
        segment,
        format!(
            "{SEGMENT_START}\norigin: user\ncontent_digest: {digest}\n\nstep one\n{SEGMENT_END}\n"
        )
    );
    let imported = render_skill_segment(&manifest.body, SkillOrigin::Imported, &digest);
    assert!(imported.contains("origin: imported\n"));
    assert!(imported.starts_with(SEGMENT_START));
    assert!(imported.ends_with(&format!("{SEGMENT_END}\n")));
    assert_ne!(segment, imported);
    let marked = format!("before {SEGMENT_END} after");
    let escaped = render_skill_segment(&marked, SkillOrigin::User, &digest);
    assert_eq!(escaped.matches(SEGMENT_END).count(), 1);
    assert!(escaped.contains(SEGMENT_END_ESCAPED));
}

fn check(
    eval_name: &str,
    eval_jsonl: &str,
    body: &str,
) -> Result<TextConsistencyReport, SkillError> {
    let digest = parsed("", body).content_digest();
    check_text_consistency(eval_name, eval_jsonl, body, SkillOrigin::User, &digest)
}

#[test]
fn exact_compares_the_rendered_segment() {
    let body = "step one";
    let manifest = parsed("", body);
    let segment = render_skill_segment(
        &manifest.body,
        SkillOrigin::User,
        &manifest.content_digest(),
    );
    let report = check(
        "evals/check.jsonl",
        &format!(r#"{{"input":"noise","expect":{segment:?},"judge":"exact"}}"#),
        body,
    )
    .unwrap();
    assert!(report.cases[0].passed);
    assert_eq!(report.renderer_version, RENDERER_VERSION);
    assert_eq!(report.consistent_counts(), (1, 1));
    assert!(report.failure_ids().is_empty());

    let raw = check(
        "evals/check.jsonl",
        r#"{"input":"","expect":"step one","judge":"exact"}"#,
        body,
    )
    .unwrap();
    assert!(!raw.cases[0].passed);
}

#[test]
fn contains_passes_on_a_segment_substring() {
    let report = check(
        "evals/check.jsonl",
        r#"{"input":"unrelated","expect":"step one","judge":"contains"}"#,
        "step one",
    )
    .unwrap();
    assert!(report.cases[0].passed);
    assert_eq!(report.cases[0].expect_digest, sha256_hex("step one"));
}

#[test]
fn expect_that_appears_only_in_input_fails() {
    let eval_name = "evals/check.jsonl";
    let input = "SECRET_NEEDLE";
    let expect = "SECRET_NEEDLE";
    let jsonl = format!(r#"{{"input":"{input}","expect":"{expect}","judge":"contains"}}"#);
    let report = check(eval_name, &jsonl, "step one").unwrap();
    assert!(!report.cases[0].passed);
    assert_eq!(
        report.cases[0].id,
        case_id(eval_name, "contains", input, expect)
    );
    assert_eq!(report.failure_ids(), vec![report.cases[0].id.as_str()]);
    assert_ne!(report.cases[0].id, format!("{eval_name}:1"));
    assert_eq!(report.consistent_counts(), (0, 1));
}

#[test]
fn failure_ids_follow_the_case_summary_when_lines_move() {
    let eval_name = "evals/check.jsonl";
    let case = r#"{"input":"SECRET_NEEDLE","expect":"SECRET_NEEDLE","judge":"contains"}"#;
    let first = check(eval_name, case, "step one").unwrap();
    let shifted = check(eval_name, &format!("\n\n{case}\n"), "step one").unwrap();
    let spaced = check(
        eval_name,
        "{ \"input\" : \"SECRET_NEEDLE\" , \"expect\" : \"SECRET_NEEDLE\" , \"judge\" : \"contains\" }\n",
        "step one",
    )
    .unwrap();
    assert_eq!(first.failure_ids(), shifted.failure_ids());
    assert_eq!(first.failure_ids(), spaced.failure_ids());
    assert_eq!(first.cases[0].line, 1);
    assert_eq!(shifted.cases[0].line, 3);
    assert_eq!(first.cases[0].id, shifted.cases[0].id);
    let other_file = check("evals/other.jsonl", case, "step one").unwrap();
    assert_ne!(first.failure_ids(), other_file.failure_ids());
}

#[test]
fn duplicate_eval_cases_are_refused() {
    let eval_name = "evals/check.jsonl";
    let case = r#"{"input":"x","expect":"step one","judge":"contains"}"#;
    let err = check(eval_name, &format!("{case}\n{case}\n"), "step one").unwrap_err();
    assert_eq!(err.code(), "skill_eval_invalid");
    assert!(err.to_string().contains("duplicate eval case"));
    let spaced = format!(
        "{case}\n{{ \"input\" : \"x\" , \"expect\" : \"step one\" , \"judge\" : \"contains\" }}\n"
    );
    assert_eq!(
        check(eval_name, &spaced, "step one").unwrap_err().code(),
        "skill_eval_invalid"
    );
    let report = check(eval_name, &format!("\n{case}\n"), "step one").unwrap();
    assert_eq!(report.cases[0].line, 2);
    assert_eq!(
        report.cases[0].id,
        case_id(eval_name, "contains", "x", "step one")
    );
}

#[test]
fn duplicate_json_keys_are_refused() {
    let err = check(
        "evals/check.jsonl",
        r#"{"input":"x","expect":"absent","judge":"contains","expect":"step one"}"#,
        "step one",
    )
    .unwrap_err();
    assert_eq!(err.code(), "skill_eval_invalid");
    assert!(err.to_string().contains("duplicate key expect"));
}

#[test]
fn a_leading_bom_is_refused() {
    let markdown = format!("\u{feff}{}", skill("", "x"));
    assert_eq!(code_of(&markdown), "skill_manifest_invalid");
    assert!(detail(&markdown).contains("文件以 BOM 开头，请去掉"));
    let eval = format!(
        "\u{feff}{}",
        r#"{"input":"x","expect":"step one","judge":"contains"}"#
    );
    let err = check("evals/check.jsonl", &eval, "step one").unwrap_err();
    assert_eq!(err.code(), "skill_eval_invalid");
    assert!(err.to_string().contains("文件以 BOM 开头，请去掉"));
}

#[test]
fn imported_community_file_fills_defaults_and_user_parse_rejects_it() {
    let markdown = "---\nname: weekly-report\ndescription: 周报\n---\nhello\n";
    assert_eq!(code_of(markdown), "skill_manifest_invalid");
    let manifest = parse_imported(markdown).unwrap();
    assert_eq!(manifest.version, "0.1.0");
    assert!(manifest.requires.is_empty());
    assert_eq!(manifest.mode, SkillMode::Manual);
    assert_eq!(manifest.evals, "");
    assert_eq!(manifest.description.text.as_deref(), Some("周报"));
    assert_eq!(manifest.body, "hello\n");
    assert_eq!(manifest.name, "weekly-report");
    let full = skill("", "hello");
    assert_eq!(
        parse_skill_markdown(&full).unwrap(),
        parse_imported(&full).unwrap()
    );
    let proactive = "---\nname: weekly-report\ndescription: 周报\nmode: proactive\n---\nhello\n";
    assert_eq!(
        parse_imported(proactive).unwrap_err().code(),
        "skill_proactive_unsupported"
    );
}

#[test]
fn empty_expect_is_invalid_for_contains_and_exact() {
    let contains = check(
        "evals/check.jsonl",
        r#"{"input":"x","expect":"","judge":"contains"}"#,
        "step one",
    );
    assert_eq!(contains.unwrap_err().code(), "skill_eval_invalid");
    let exact = check(
        "evals/check.jsonl",
        r#"{"input":"x","expect":"","judge":"exact"}"#,
        "step one",
    );
    assert_eq!(exact.unwrap_err().code(), "skill_eval_invalid");
}

#[test]
fn local_model_unknown_fields_and_an_empty_file_are_refused() {
    assert_eq!(
        check(
            "evals/check.jsonl",
            r#"{"input":"x","expect":"step","judge":"local_model"}"#,
            "step"
        )
        .unwrap_err()
        .code(),
        "skill_eval_invalid"
    );
    assert_eq!(
        check(
            "evals/check.jsonl",
            r#"{"input":"x","expect":"step","judge":"contains","note":"hide"}"#,
            "step"
        )
        .unwrap_err()
        .code(),
        "skill_eval_invalid"
    );
    assert_eq!(
        check("evals/check.jsonl", "", "step").unwrap_err().code(),
        "skill_eval_empty"
    );
    assert_eq!(
        check("evals/check.jsonl", "\n\n", "step")
            .unwrap_err()
            .code(),
        "skill_eval_empty"
    );
    assert_eq!(
        check(
            "",
            r#"{"input":"x","expect":"step","judge":"contains"}"#,
            "step"
        )
        .unwrap_err()
        .code(),
        "skill_eval_invalid"
    );
}

#[test]
fn an_eval_document_over_32_kib_is_too_large() {
    let mut jsonl = String::from(r#"{"input":"x","expect":"step","judge":"contains"}"#);
    jsonl.push_str(&" ".repeat(BODY_MAX_BYTES));
    assert!(jsonl.len() > BODY_MAX_BYTES);
    assert_eq!(
        check("evals/check.jsonl", &jsonl, "step")
            .unwrap_err()
            .code(),
        "skill_body_too_large"
    );
}

#[test]
fn a_mixed_report_counts_passes_and_binds_failure_ids() {
    let body = "step one";
    let manifest = parsed("", body);
    let segment = render_skill_segment(
        &manifest.body,
        SkillOrigin::User,
        &manifest.content_digest(),
    );
    let jsonl = format!(
        "{{\"input\":\"\",\"expect\":{segment:?},\"judge\":\"exact\"}}\n{{\"input\":\"SECRET_NEEDLE\",\"expect\":\"SECRET_NEEDLE\",\"judge\":\"contains\"}}\n"
    );
    let report = check("evals/check.jsonl", &jsonl, body).unwrap();
    assert_eq!(report.consistent_counts(), (1, 2));
    assert_eq!(report.failure_ids().len(), 1);
    assert_eq!(report.cases[1].id, report.failure_ids()[0]);
    assert!(!report.segment_digest.is_empty());
}
