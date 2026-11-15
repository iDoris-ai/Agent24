//! `agent24 skill`：技能列表、详情、导入、重扫，以及经桌面确认的激活。
//!
//! 只打 REST，不打开库。导入读的是本地文件内容，请求体里没有路径。
//! 激活的第一次 POST 不带提升头：daemon 对业务错误和 `unchanged` 直接回答，
//! 只有 `presence_required` 才由 [`super::send_elevating`] 去请求桌面确认并重发。
//! 先带假提升头，或跳过这次预检直接 `elevate`，集成测试会红。

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use clap::Subcommand;
use serde_json::{Map, Value, json};

const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Subcommand)]
pub(crate) enum SkillAction {
    /// 列出已安装的技能
    List,
    /// 查看一个技能的指针、修订和激活历史（不含正文）
    Show {
        /// 技能 id
        id: String,
    },
    /// 读取本地 SKILL.md（以及可选的评测文件），把正文提交给 daemon
    Install {
        /// SKILL.md 的本地路径。daemon 只收到正文，不收到这个路径
        path: std::path::PathBuf,
        /// 评测文件的本地路径。提交时的名字用清单里声明的相对路径，不用这个路径
        #[arg(long)]
        evals: Option<std::path::PathBuf>,
    },
    /// 重扫用户技能目录
    Rescan,
    /// 激活一对修订，或回滚到一条历史。指针真会移动时才请求桌面确认。
    /// 省略 --body / --evals 时，有指针就只换比指针新的一侧；两侧都新则拒绝。
    /// 没有指针时，该来源恰好一条正文和一条评测才直接用。
    /// 两个来源都有修订时，用 --origin user|imported，或同时给出 --body 与 --evals
    Activate {
        /// 技能 id
        id: String,
        /// 正文修订 id。省略则按指针或唯一修订选择。须与 --evals 同时给出
        #[arg(long)]
        body: Option<String>,
        /// 评测修订 id。省略则按指针或唯一修订选择。须与 --body 同时给出
        #[arg(long)]
        evals: Option<String>,
        /// 接受的失败用例 id，可重复。须正好是服务端这一次评测的失败用例
        #[arg(long = "accept")]
        accept: Vec<String>,
        /// 回滚到这条激活历史。不能和 --body / --evals / --accept / --origin 一起用
        #[arg(long)]
        rollback: Option<String>,
        /// 省略修订 id 时只在这个来源里选择。取值 user 或 imported
        #[arg(long, value_parser = ["user", "imported"])]
        origin: Option<String>,
    },
}

pub(crate) async fn run(action: SkillAction, json_output: bool) -> Result<(), String> {
    if let Some(message) = local_rejection(&action) {
        return fail(json_output, None, message);
    }
    let ep = super::connect().await?;
    let result = dispatch(&ep, action, json_output).await;
    super::finish(ep).await;
    result
}

fn local_rejection(action: &SkillAction) -> Option<String> {
    match action {
        SkillAction::Show { id } | SkillAction::Activate { id, .. } if id.trim().is_empty() => {
            Some("invalid_request: 技能 id 不能为空".to_owned())
        }
        SkillAction::Activate {
            body,
            evals,
            accept,
            rollback,
            origin,
            ..
        } => activate_flags(
            body.as_deref(),
            evals.as_deref(),
            accept,
            rollback.as_deref(),
            origin.as_deref(),
        ),
        _ => None,
    }
}

/// 回滚、显式修订、按来源取最新，三种不能混用。
fn activate_flags(
    body: Option<&str>,
    evals: Option<&str>,
    accept: &[String],
    rollback: Option<&str>,
    origin: Option<&str>,
) -> Option<String> {
    let pair = body.is_some() || evals.is_some();
    if rollback.is_some() && (pair || !accept.is_empty() || origin.is_some()) {
        return Some("invalid_request: 回滚不能同时带修订、--origin 或 --accept".to_owned());
    }
    if body.is_some() != evals.is_some() {
        return Some("invalid_request: --body 和 --evals 要一起给出".to_owned());
    }
    if origin.is_some() && pair {
        return Some("invalid_request: --origin 不能和 --body / --evals 一起用".to_owned());
    }
    if let Some(origin) = origin
        && origin != "user"
        && origin != "imported"
    {
        return Some("invalid_request: --origin 只能是 user 或 imported".to_owned());
    }
    None
}

async fn dispatch(
    ep: &super::Endpoint,
    action: SkillAction,
    json_output: bool,
) -> Result<(), String> {
    match action {
        SkillAction::List => list(ep, json_output).await,
        SkillAction::Show { id } => show(ep, &id, json_output).await,
        SkillAction::Install { path, evals } => {
            install(ep, &path, evals.as_deref(), json_output).await
        }
        SkillAction::Rescan => rescan(ep, json_output).await,
        SkillAction::Activate {
            id,
            body,
            evals,
            accept,
            rollback,
            origin,
        } => {
            activate(
                ep,
                &id,
                ActivateChoice {
                    body: body.as_deref(),
                    evals: evals.as_deref(),
                    accept: &accept,
                    rollback: rollback.as_deref(),
                    origin: origin.as_deref(),
                },
                json_output,
            )
            .await
        }
    }
}

async fn list(ep: &super::Endpoint, json_output: bool) -> Result<(), String> {
    let (status, text) = exchange(ep, reqwest::Method::GET, "/api/v1/skills", None).await?;
    if !status.is_success() {
        return fail(json_output, Some(&text), explain_failure(&text));
    }
    if json_output {
        println!("{text}");
        return Ok(());
    }
    let body: Value = parse_json(&text)?;
    let skills = body["skills"]
        .as_array()
        .ok_or_else(|| "invalid_response: 列表没有 skills".to_owned())?;
    if skills.is_empty() {
        println!("(none)");
        return Ok(());
    }
    let mut details = BTreeMap::new();
    for skill in skills {
        let Some(id) = skill["skill_id"].as_str() else {
            return Err("invalid_response: 列表行没有 skill_id".to_owned());
        };
        if details.contains_key(id) {
            continue;
        }
        let (status, text) = exchange(
            ep,
            reqwest::Method::GET,
            &format!("/api/v1/skills/{}", super::qs_encode(id)),
            None,
        )
        .await?;
        if !status.is_success() {
            return fail(json_output, Some(&text), explain_failure(&text));
        }
        details.insert(id.to_owned(), parse_json(&text)?);
    }
    println!("名称  semver  来源  指针  文本一致性");
    for skill in skills {
        let id = skill["skill_id"].as_str().unwrap_or("");
        let origin = skill["origin"].as_str().unwrap_or("");
        let detail = details.get(id);
        println!(
            "{}  {}  {}  {}  {}",
            id,
            skill["semver"].as_str().unwrap_or(""),
            origin,
            pointer_cell(detail, origin),
            consistency_cell(detail, origin)
        );
    }
    Ok(())
}

async fn show(ep: &super::Endpoint, id: &str, json_output: bool) -> Result<(), String> {
    let (status, text) = exchange(
        ep,
        reqwest::Method::GET,
        &format!("/api/v1/skills/{}", super::qs_encode(id)),
        None,
    )
    .await?;
    if !status.is_success() {
        return fail(json_output, Some(&text), explain_failure(&text));
    }
    if json_output {
        println!("{text}");
        return Ok(());
    }
    println!("{}", show_text(&parse_json(&text)?));
    Ok(())
}

async fn install(
    ep: &super::Endpoint,
    path: &Path,
    evals: Option<&Path>,
    json_output: bool,
) -> Result<(), String> {
    let markdown = read_text(path)?;
    let declared = declared_eval_name(&markdown)?;
    let evals_body = match (declared.as_deref(), evals) {
        (None, None) => None,
        (Some(name), Some(file)) => Some(json!([{ "name": name, "content": read_text(file)? }])),
        (Some(name), None) => {
            return fail(
                json_output,
                None,
                format!("invalid_request: 清单声明了 {name}，请用 --evals 传入该文件"),
            );
        }
        (None, Some(_)) => {
            return fail(
                json_output,
                None,
                "invalid_request: 清单没有声明评测路径，不能附带 --evals".to_owned(),
            );
        }
    };
    let mut payload = Map::new();
    payload.insert("markdown".to_owned(), json!(markdown));
    if let Some(evals_body) = evals_body {
        payload.insert("evals".to_owned(), evals_body);
    }
    let bytes = serde_json::to_vec(&Value::Object(payload)).map_err(|err| err.to_string())?;
    let (status, text) = exchange(ep, reqwest::Method::POST, "/api/v1/skills", Some(bytes)).await?;
    finish_json(json_output, status, &text, install_text)
}

async fn rescan(ep: &super::Endpoint, json_output: bool) -> Result<(), String> {
    let (status, text) = exchange(
        ep,
        reqwest::Method::POST,
        "/api/v1/skills/scans",
        Some(b"{}".to_vec()),
    )
    .await?;
    finish_json(json_output, status, &text, scan_text)
}

struct ActivateChoice<'a> {
    body: Option<&'a str>,
    evals: Option<&'a str>,
    accept: &'a [String],
    rollback: Option<&'a str>,
    origin: Option<&'a str>,
}

async fn activate(
    ep: &super::Endpoint,
    id: &str,
    choice: ActivateChoice<'_>,
    json_output: bool,
) -> Result<(), String> {
    let ActivateChoice {
        body: body_rev,
        evals: evals_rev,
        accept,
        rollback,
        origin,
    } = choice;
    let (status, text) = exchange(
        ep,
        reqwest::Method::GET,
        &format!("/api/v1/skills/{}", super::qs_encode(id)),
        None,
    )
    .await?;
    if !status.is_success() {
        return fail(json_output, Some(&text), explain_failure(&text));
    }
    let detail = parse_json(&text)?;
    let generation = detail["generation"]
        .as_i64()
        .ok_or_else(|| "invalid_response: 详情没有代次".to_owned())?;
    let (body_rev, evals_rev) = if rollback.is_some() {
        (None, None)
    } else if let (Some(body_rev), Some(evals_rev)) = (body_rev, evals_rev) {
        (Some(body_rev.to_owned()), Some(evals_rev.to_owned()))
    } else {
        let (body, evals) = default_pair(&detail, origin)?;
        (Some(body), Some(evals))
    };
    if let (Some(body), Some(evals)) = (&body_rev, &evals_rev) {
        // 请求之前打出这一对。桌面确认看的是服务端摘要，这里先让命令行对上号。
        eprintln!("{}", activation_pair_line(&detail, body, evals));
    }
    let bytes = encode_activate(ActivateSpec {
        expected_generation: generation,
        body_revision_id: body_rev,
        evals_revision_id: evals_rev,
        accepted: accept.to_vec(),
        rollback_activation_id: rollback.map(str::to_owned),
    })?;
    // 不带提升头。send_elevating 只在 presence_required 时才 elevate 并重发。
    let (status, text) = exchange(
        ep,
        reqwest::Method::POST,
        &format!("/api/v1/skills/{}/activate", super::qs_encode(id)),
        Some(bytes),
    )
    .await?;
    finish_json(json_output, status, &text, activate_text)
}

struct ActivateSpec {
    expected_generation: i64,
    body_revision_id: Option<String>,
    evals_revision_id: Option<String>,
    accepted: Vec<String>,
    rollback_activation_id: Option<String>,
}

fn encode_activate(spec: ActivateSpec) -> Result<Vec<u8>, String> {
    let mut payload = Map::new();
    payload.insert(
        "expected_generation".to_owned(),
        json!(spec.expected_generation),
    );
    if let Some(id) = spec.body_revision_id {
        payload.insert("body_revision_id".to_owned(), json!(id));
    }
    if let Some(id) = spec.evals_revision_id {
        payload.insert("evals_revision_id".to_owned(), json!(id));
    }
    if !spec.accepted.is_empty() {
        payload.insert("accepted".to_owned(), json!(spec.accepted));
    }
    if let Some(id) = spec.rollback_activation_id {
        payload.insert("rollback_activation_id".to_owned(), json!(id));
    }
    serde_json::to_vec(&Value::Object(payload)).map_err(|err| err.to_string())
}

/// 不带 `--body` / `--evals` 时的那一对。来源仍是最新正文的来源；两个来源都有
/// 修订且没有 `--origin` 时不拼一对。
///
/// 有这个来源的指针时只换一侧：比指针新的正文配指针上的评测，否则比指针新的评测
/// 配指针上的正文。两侧都比指针新就拒绝，因为 daemon 同一次激活也只许换一侧。
/// 没有指针时，这个来源恰好一条正文和一条评测才直接用。分别取两侧最新会在
/// 重装复用旧 `created_at` 时拼出从未一起装过的一对，这里不这么做。
fn default_pair(detail: &Value, origin: Option<&str>) -> Result<(String, String), String> {
    let rows = detail["revisions"]
        .as_array()
        .ok_or_else(|| "invalid_response: 详情没有修订".to_owned())?;
    let chosen = match origin {
        Some(origin) => origin.to_owned(),
        None => {
            let mut seen = BTreeSet::new();
            for row in rows {
                if let Some(origin) = row["origin"].as_str()
                    && (origin == "user" || origin == "imported")
                {
                    seen.insert(origin);
                }
            }
            if seen.len() > 1 {
                return Err(
                    "invalid_request: 这个技能在 user 和 imported 都有修订。请用 --origin user 或 --origin imported，或同时给出 --body 与 --evals"
                        .to_owned(),
                );
            }
            latest_body_origin(rows)?
        }
    };
    let bodies = kind_rows(detail, "body", &chosen)?;
    let evals = kind_rows(detail, "evals", &chosen)?;
    if bodies.is_empty() {
        return Err("invalid_request: 详情里没有正文修订".to_owned());
    }
    if evals.is_empty() {
        return Err("invalid_request: 详情里没有评测修订".to_owned());
    }
    if let Some((body_id, evals_id)) = pointer_ids(detail, &chosen) {
        let newer_body = newer_than(&bodies, body_id)?;
        let newer_evals = newer_than(&evals, evals_id)?;
        return match (newer_body, newer_evals) {
            (Some(_), Some(_)) => Err(
                "invalid_request: 正文和评测都比当前指针新。请分两次激活，每次用 --body 与 --evals 只换一侧"
                    .to_owned(),
            ),
            (Some(body), None) => Ok((body, evals_id.to_owned())),
            (None, Some(evals)) => Ok((body_id.to_owned(), evals)),
            (None, None) => Ok((body_id.to_owned(), evals_id.to_owned())),
        };
    }
    if bodies.len() == 1 && evals.len() == 1 {
        return Ok((bodies[0].1.clone(), evals[0].1.clone()));
    }
    Err(
        "invalid_request: 没有激活指针，且这个来源有多条正文或评测。请用 --body 与 --evals 指定"
            .to_owned(),
    )
}

/// 这个来源的指针。别的来源、空 id、或还没有指针，都当没有。
fn pointer_ids<'a>(detail: &'a Value, origin: &str) -> Option<(&'a str, &'a str)> {
    let pointer = detail.get("pointer")?;
    if !pointer.is_object() || pointer["origin"].as_str() != Some(origin) {
        return None;
    }
    let body = pointer["body_revision_id"].as_str()?;
    let evals = pointer["evals_revision_id"].as_str()?;
    if body.is_empty() || evals.is_empty() {
        return None;
    }
    Some((body, evals))
}

/// `(created_at, revision_id)`，只留选定来源、选定种类、id 非空的行。
fn kind_rows(detail: &Value, kind: &str, origin: &str) -> Result<Vec<(String, String)>, String> {
    let rows = detail["revisions"]
        .as_array()
        .ok_or_else(|| "invalid_response: 详情没有修订".to_owned())?;
    let mut out = Vec::new();
    for row in rows {
        if row["kind"].as_str() != Some(kind) || row["origin"].as_str() != Some(origin) {
            continue;
        }
        let Some(id) = row["revision_id"].as_str().filter(|id| !id.is_empty()) else {
            continue;
        };
        out.push((
            row["created_at"].as_str().unwrap_or("").to_owned(),
            id.to_owned(),
        ));
    }
    Ok(out)
}

/// 比指针这一行更新的那条。比较键与库的列表顺序一样：`created_at`，再 `revision_id`。
/// 重装复用的旧行不会因此变成新的。
fn newer_than(rows: &[(String, String)], current_id: &str) -> Result<Option<String>, String> {
    let current = rows
        .iter()
        .find(|(_, id)| id == current_id)
        .ok_or_else(|| "invalid_response: 指针上的修订不在这个来源里".to_owned())?;
    let mut best: Option<&(String, String)> = None;
    for row in rows {
        if (row.0.as_str(), row.1.as_str()) <= (current.0.as_str(), current.1.as_str()) {
            continue;
        }
        let replace = best.is_none_or(|prev| {
            (row.0.as_str(), row.1.as_str()) >= (prev.0.as_str(), prev.1.as_str())
        });
        if replace {
            best = Some(row);
        }
    }
    Ok(best.map(|(_, id)| id.clone()))
}

/// 发请求前打出的那一对：修订 id 和摘要各前 8 位，semver 原样。
fn activation_pair_line(detail: &Value, body_id: &str, evals_id: &str) -> String {
    let (body_ver, body_digest) = revision_brief(detail, body_id);
    let (evals_ver, evals_digest) = revision_brief(detail, evals_id);
    format!(
        "将激活 正文 {} {} {}  评测 {} {} {}",
        prefix8(body_id),
        body_ver,
        prefix8(&body_digest),
        prefix8(evals_id),
        evals_ver,
        prefix8(&evals_digest)
    )
}

fn revision_brief(detail: &Value, id: &str) -> (String, String) {
    let Some(rows) = detail["revisions"].as_array() else {
        return ("—".to_owned(), String::new());
    };
    for row in rows {
        if row["revision_id"].as_str() == Some(id) {
            return (
                row["semver"].as_str().unwrap_or("—").to_owned(),
                row["content_digest"].as_str().unwrap_or("").to_owned(),
            );
        }
    }
    ("—".to_owned(), String::new())
}

fn latest_body_origin(rows: &[Value]) -> Result<String, String> {
    let mut best: Option<(&str, &str, &str)> = None;
    for row in rows {
        if row["kind"].as_str() != Some("body") {
            continue;
        }
        let created = row["created_at"].as_str().unwrap_or("");
        let id = row["revision_id"].as_str().unwrap_or("");
        let origin = row["origin"].as_str().unwrap_or("");
        let replace = best.is_none_or(|(at, prev, _)| (created, id) >= (at, prev));
        if replace {
            best = Some((created, id, origin));
        }
    }
    match best {
        Some((_, _, origin)) if origin == "user" || origin == "imported" => Ok(origin.to_owned()),
        Some(_) => Err("invalid_response: 最新正文没有来源".to_owned()),
        None => Err("invalid_request: 详情里没有正文修订".to_owned()),
    }
}

async fn exchange(
    ep: &super::Endpoint,
    method: reqwest::Method,
    path: &str,
    body: Option<Vec<u8>>,
) -> Result<(reqwest::StatusCode, String), String> {
    let reply = super::send_elevating(ep, method, path, body, TIMEOUT, true)
        .await
        .map_err(send_error)?;
    let status = reply.status;
    let text = match reply.body {
        super::ReplyBody::Buffered(body) => {
            String::from_utf8(body).map_err(|_| "invalid_response: 响应不是 UTF-8".to_owned())?
        }
        super::ReplyBody::Legacy(response) => {
            response.text().await.map_err(|err| err.to_string())?
        }
    };
    Ok((status, text))
}

fn send_error(err: super::SendError) -> String {
    let (code, message) = err.parts();
    format!("{code}: {message}")
}

fn finish_json(
    json_output: bool,
    status: reqwest::StatusCode,
    text: &str,
    human: fn(&Value) -> String,
) -> Result<(), String> {
    if !status.is_success() {
        return fail(json_output, Some(text), explain_failure(text));
    }
    if json_output {
        println!("{text}");
        return Ok(());
    }
    println!("{}", human(&parse_json(text)?));
    Ok(())
}

fn fail(json_output: bool, raw: Option<&str>, message: String) -> Result<(), String> {
    let message = terminal_text(&message);
    if json_output {
        match raw.map(str::trim).filter(|text| !text.is_empty()) {
            Some(text) => println!("{text}"),
            None => {
                let (code, rest) = message
                    .split_once(": ")
                    .unwrap_or(("invalid_request", &message));
                println!("{}", json!({"error": {"code": code, "message": rest}}));
            }
        }
    }
    Err(message)
}

fn parse_json(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|_| "invalid_response: 响应不是 JSON".to_owned())
}

fn explain_failure(text: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return format!("invalid_response: {}", terminal_text(text));
    };
    let code = terminal_text(
        value["error"]["code"]
            .as_str()
            .unwrap_or("invalid_response"),
    );
    let message = terminal_text(value["error"]["message"].as_str().unwrap_or(""));
    let mut out = format!("{code}: {message}");
    if let Some(cases) = value["error"]["details"]["cases"].as_array() {
        out.push_str("\n用例:");
        for case in cases {
            let passed = case["passed"].as_bool().unwrap_or(false);
            let digest = terminal_text(case["expect_digest"].as_str().unwrap_or(""));
            out.push_str(&format!(
                "\n  {} 行 {} {} passed={passed} 摘要 {}",
                terminal_text(case["id"].as_str().unwrap_or("")),
                case["line"].as_i64().unwrap_or(0),
                terminal_text(case["judge"].as_str().unwrap_or("")),
                prefix8(&digest)
            ));
        }
    }
    if let Some(failed) = value["error"]["details"]["failed"].as_array() {
        let flags = failed
            .iter()
            .filter_map(Value::as_str)
            .map(|id| format!("--accept {}", terminal_text(id)))
            .collect::<Vec<_>>()
            .join(" ");
        if !flags.is_empty() {
            out.push_str(&format!("\n失败: {flags}"));
        }
    }
    out
}

/// 打到终端之前去掉 C0/C1（ESC、BEL、DEL 等）。换行留着，多行用例摘要还要它。
fn terminal_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| *ch == '\n' || !ch.is_control())
        .collect()
}

fn read_text(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path)
        .map_err(|err| format!("invalid_request: 读不到 {}: {err}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("invalid_request: {} 不是普通文件", path.display()));
    }
    let file = std::fs::File::open(path)
        .map_err(|err| format!("invalid_request: 读不到 {}: {err}", path.display()))?;
    // 正文和评测都是 daemon 的 `BODY_MAX_BYTES`。多读 1 字节只为判定超限。
    let limit = u64::try_from(agent24_skill::BODY_MAX_BYTES)
        .map_err(|_| "invalid_request: 读取上限无效".to_owned())?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("invalid_request: 读不到 {}: {err}", path.display()))?;
    if bytes.len() > agent24_skill::BODY_MAX_BYTES {
        return Err(format!(
            "invalid_request: {} 超过 {} 字节",
            path.display(),
            agent24_skill::BODY_MAX_BYTES
        ));
    }
    String::from_utf8(bytes).map_err(|_| format!("invalid_request: {} 不是 UTF-8", path.display()))
}

/// 清单里声明的评测相对路径。与 `POST /api/v1/skills` 一样走 `parse_imported`。
/// 空评测列表是 `None`。
fn declared_eval_name(markdown: &str) -> Result<Option<String>, String> {
    let manifest = agent24_skill::parse_imported(markdown).map_err(|err| err.to_string())?;
    if manifest.evals.is_empty() {
        Ok(None)
    } else {
        Ok(Some(manifest.evals))
    }
}

fn prefix8(id: &str) -> String {
    id.chars().take(8).collect()
}

fn pointer_cell(detail: Option<&Value>, origin: &str) -> String {
    let Some(pointer) = detail.and_then(|detail| detail.get("pointer")) else {
        return "未激活".to_owned();
    };
    if pointer.is_null() || pointer["origin"].as_str() != Some(origin) {
        return "未激活".to_owned();
    }
    format!(
        "{}/{}",
        prefix8(pointer["body_revision_id"].as_str().unwrap_or("")),
        prefix8(pointer["evals_revision_id"].as_str().unwrap_or(""))
    )
}

fn consistency_cell(detail: Option<&Value>, origin: &str) -> String {
    let Some(detail) = detail else {
        return "—".to_owned();
    };
    if detail["pointer"]["origin"].as_str() != Some(origin) {
        return "—".to_owned();
    }
    match (
        detail["text_consistency"]["consistent"].as_u64(),
        detail["text_consistency"]["total"].as_u64(),
    ) {
        (Some(consistent), Some(total)) => format!("{consistent}/{total}"),
        _ => "—".to_owned(),
    }
}

fn show_text(detail: &Value) -> String {
    let origin = detail["pointer"]["origin"].as_str().unwrap_or("");
    let mut lines = vec![
        format!("名称: {}", detail["skill_id"].as_str().unwrap_or("")),
        format!("代次: {}", detail["generation"].as_i64().unwrap_or(0)),
        format!("指针: {}", pointer_cell(Some(detail), origin)),
        format!("文本一致性: {}", consistency_cell(Some(detail), origin)),
        "修订:".to_owned(),
    ];
    let revisions = detail["revisions"].as_array();
    if revisions.is_none_or(Vec::is_empty) {
        lines.push("  (无)".to_owned());
    }
    if let Some(revisions) = revisions {
        for row in revisions {
            lines.push(format!(
                "  {}  {}  {}  {}  {}",
                row["revision_id"].as_str().unwrap_or(""),
                row["kind"].as_str().unwrap_or(""),
                row["origin"].as_str().unwrap_or(""),
                row["semver"].as_str().unwrap_or(""),
                prefix8(row["content_digest"].as_str().unwrap_or(""))
            ));
        }
    }
    lines.push("激活历史:".to_owned());
    let activations = detail["activations"].as_array();
    if activations.is_none_or(Vec::is_empty) {
        lines.push("  (无)".to_owned());
    }
    if let Some(activations) = activations {
        for row in activations {
            let from = row["origin_from"].as_str().unwrap_or("—");
            lines.push(format!(
                "  {}  {}  {}  {}→{}  {}/{}",
                row["activation_id"].as_str().unwrap_or(""),
                row["seq"].as_i64().unwrap_or(0),
                row["result_code"].as_str().unwrap_or(""),
                from,
                row["origin_to"].as_str().unwrap_or(""),
                prefix8(row["body_revision_id"].as_str().unwrap_or("")),
                prefix8(row["evals_revision_id"].as_str().unwrap_or(""))
            ));
        }
    }
    lines.join("\n")
}

fn install_text(value: &Value) -> String {
    let origin = value["origin"].as_str().unwrap_or("");
    format!(
        "已导入 {}  {}  {}\n正文修订: {}\n评测修订: {}\n新写入: {}\n指针: {}\n文本一致性: {}",
        value["skill_id"].as_str().unwrap_or(""),
        value["semver"].as_str().unwrap_or(""),
        origin,
        value["body_revision_id"].as_str().unwrap_or(""),
        value["evals_revision_id"].as_str().unwrap_or(""),
        if value["inserted"].as_bool().unwrap_or(false) {
            "是"
        } else {
            "否"
        },
        pointer_cell(Some(value), origin),
        count_text(&value["text_consistency"])
    )
}

fn count_text(value: &Value) -> String {
    match (value["consistent"].as_u64(), value["total"].as_u64()) {
        (Some(consistent), Some(total)) => format!("{consistent}/{total}"),
        _ => "—".to_owned(),
    }
}

fn scan_text(value: &Value) -> String {
    let skills = value["skills"].as_array().map_or(0, Vec::len);
    let errors = value["errors"].as_array();
    let mut lines = vec![format!(
        "来源: {}  写入: {skills}  错误: {}",
        value["origin"].as_str().unwrap_or(""),
        errors.map_or(0, Vec::len)
    )];
    if let Some(errors) = errors {
        for err in errors {
            lines.push(format!(
                "{}  {}: {}",
                err["path"].as_str().unwrap_or(""),
                err["code"].as_str().unwrap_or(""),
                err["message"].as_str().unwrap_or("")
            ));
        }
    }
    lines.join("\n")
}

fn activate_text(value: &Value) -> String {
    let origin = value["pointer"]["origin"].as_str().unwrap_or("");
    let mut lines = vec![
        value["result"].as_str().unwrap_or("").to_owned(),
        format!("名称: {}", value["skill_id"].as_str().unwrap_or("")),
        format!("代次: {}", value["generation"].as_i64().unwrap_or(0)),
        format!("指针: {}", pointer_cell(Some(value), origin)),
        format!("文本一致性: {}", count_text(&value["text_consistency"])),
    ];
    if let Some(note) = value["note"].as_str() {
        lines.push(note.to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn declared_eval_name_uses_the_manifest_path() {
        let markdown =
            "---\nname: weekly-report\ndescription: d\nevals: \"evals/check.jsonl\"\n---\nhello\n";
        assert_eq!(
            declared_eval_name(markdown).unwrap().as_deref(),
            Some("evals/check.jsonl")
        );
        assert_eq!(
            declared_eval_name("---\nname: weekly-report\ndescription: d\nevals: []\n---\n")
                .unwrap(),
            None
        );
        assert!(declared_eval_name("---\nname: weekly-report\n").is_err());
    }

    #[test]
    fn mixed_activate_flags_are_rejected_before_any_request() {
        assert!(activate_flags(None, None, &[], Some("act"), None).is_none());
        assert!(activate_flags(Some("b"), Some("e"), &[], None, None).is_none());
        assert!(activate_flags(None, None, &[], None, Some("user")).is_none());
        assert!(
            activate_flags(Some("b"), None, &[], None, None)
                .unwrap()
                .contains("invalid_request")
        );
        assert!(
            activate_flags(None, None, &["case".to_owned()], Some("act"), None)
                .unwrap()
                .contains("invalid_request")
        );
        assert!(
            activate_flags(None, None, &[], Some("act"), Some("user"))
                .unwrap()
                .contains("--origin")
        );
        assert!(
            activate_flags(Some("b"), Some("e"), &[], None, Some("imported"))
                .unwrap()
                .contains("--origin")
        );
        // 去掉上面的 `pair ||` 后，回滚加 `--body`/`--evals` 会漏下去，断言变红。
        assert!(
            activate_flags(Some("b"), Some("e"), &[], Some("act"), None)
                .unwrap()
                .contains("回滚")
        );
    }

    #[test]
    fn default_pair_does_not_cross_origins() {
        let detail = json!({
            "revisions": [
                {"kind": "body", "origin": "user", "created_at": "t1", "revision_id": "user-body"},
                {"kind": "evals", "origin": "user", "created_at": "t3", "revision_id": "user-evals"},
                {"kind": "body", "origin": "imported", "created_at": "t2", "revision_id": "imp-body"},
                {"kind": "evals", "origin": "imported", "created_at": "t0", "revision_id": "imp-evals"}
            ]
        });
        let err = default_pair(&detail, None).unwrap_err();
        assert!(err.contains("--origin"), "{err}");
        assert!(err.contains("--body"), "{err}");
        assert!(err.contains("--evals"), "{err}");
        assert_eq!(
            default_pair(&detail, Some("imported")).unwrap(),
            ("imp-body".to_owned(), "imp-evals".to_owned())
        );
        assert_eq!(
            default_pair(&detail, Some("user")).unwrap(),
            ("user-body".to_owned(), "user-evals".to_owned())
        );

        let only = json!({
            "revisions": [
                {"kind": "body", "origin": "imported", "created_at": "t1", "revision_id": "b1"},
                {"kind": "evals", "origin": "imported", "created_at": "t2", "revision_id": "e2"},
                {"kind": "body", "origin": "imported", "created_at": "t2", "revision_id": "b2"},
                {"kind": "evals", "origin": "imported", "created_at": "t1", "revision_id": "e1"}
            ]
        });
        let err = default_pair(&only, None).unwrap_err();
        assert!(err.contains("--body"), "{err}");
        assert!(err.contains("--evals"), "{err}");
    }

    /// (A,E1) → (B,E2) → 重装 A 得到 E3。A 的 `created_at` 仍是 t1，分别取两侧
    /// 最新会得到从未一起装过的 (B,E3)。改回那种选择，下面的断言变红。
    #[test]
    fn default_pair_does_not_assemble_the_reinstall_with_the_newest_evals() {
        let detail = reinstall_detail(json!({
            "origin": "user",
            "body_revision_id": "A",
            "evals_revision_id": "E1"
        }));
        let err = default_pair(&detail, None).unwrap_err();
        assert_ne!(
            default_pair(&detail, None).ok(),
            Some(("B".to_owned(), "E3".to_owned()))
        );
        assert!(err.contains("分两次"), "{err}");
        assert!(err.contains("--body") && err.contains("--evals"), "{err}");

        let bare = reinstall_detail(Value::Null);
        assert_ne!(
            default_pair(&bare, None).ok(),
            Some(("B".to_owned(), "E3".to_owned()))
        );
        let err = default_pair(&bare, None).unwrap_err();
        assert!(err.contains("没有激活指针"), "{err}");
        assert!(err.contains("--body") && err.contains("--evals"), "{err}");
    }

    #[test]
    fn default_pair_moves_only_the_newer_side() {
        let body_only = json!({
            "pointer": {"origin": "user", "body_revision_id": "A", "evals_revision_id": "E1"},
            "revisions": [
                {"kind": "body", "origin": "user", "created_at": "t1", "revision_id": "A"},
                {"kind": "evals", "origin": "user", "created_at": "t1", "revision_id": "E1"},
                {"kind": "body", "origin": "user", "created_at": "t2", "revision_id": "B"},
                {"kind": "body", "origin": "user", "created_at": "t3", "revision_id": "C"}
            ]
        });
        assert_eq!(
            default_pair(&body_only, None).unwrap(),
            ("C".to_owned(), "E1".to_owned())
        );

        let evals_only = json!({
            "pointer": {"origin": "user", "body_revision_id": "A", "evals_revision_id": "E1"},
            "revisions": [
                {"kind": "body", "origin": "user", "created_at": "t1", "revision_id": "A"},
                {"kind": "evals", "origin": "user", "created_at": "t1", "revision_id": "E1"},
                {"kind": "evals", "origin": "user", "created_at": "t2", "revision_id": "E2"}
            ]
        });
        assert_eq!(
            default_pair(&evals_only, None).unwrap(),
            ("A".to_owned(), "E2".to_owned())
        );

        let same = json!({
            "pointer": {"origin": "user", "body_revision_id": "A", "evals_revision_id": "E1"},
            "revisions": [
                {"kind": "body", "origin": "user", "created_at": "t1", "revision_id": "A"},
                {"kind": "evals", "origin": "user", "created_at": "t1", "revision_id": "E1"}
            ]
        });
        assert_eq!(
            default_pair(&same, None).unwrap(),
            ("A".to_owned(), "E1".to_owned())
        );

        let alone = json!({
            "pointer": null,
            "revisions": [
                {"kind": "body", "origin": "user", "created_at": "t1", "revision_id": "A"},
                {"kind": "evals", "origin": "user", "created_at": "t1", "revision_id": "E1"}
            ]
        });
        assert_eq!(
            default_pair(&alone, None).unwrap(),
            ("A".to_owned(), "E1".to_owned())
        );
    }

    #[test]
    fn activation_pair_line_shows_ids_semver_and_digest() {
        let detail = json!({
            "revisions": [
                {"revision_id": "bodyrev-aaaa", "semver": "1.2.0", "content_digest": "deadbeef0011"},
                {"revision_id": "evalsrev-bbbb", "semver": "1.2.1", "content_digest": "cafebabe2233"}
            ]
        });
        assert_eq!(
            activation_pair_line(&detail, "bodyrev-aaaa", "evalsrev-bbbb"),
            "将激活 正文 bodyrev- 1.2.0 deadbeef  评测 evalsrev 1.2.1 cafebabe"
        );
    }

    fn reinstall_detail(pointer: Value) -> Value {
        json!({
            "pointer": pointer,
            "revisions": [
                {"kind": "body", "origin": "user", "created_at": "t1", "revision_id": "A", "semver": "1.0.0", "content_digest": "aaaaaaaa11"},
                {"kind": "evals", "origin": "user", "created_at": "t1", "revision_id": "E1", "semver": "1.0.0", "content_digest": "11111111aa"},
                {"kind": "body", "origin": "user", "created_at": "t2", "revision_id": "B", "semver": "1.1.0", "content_digest": "bbbbbbbb22"},
                {"kind": "evals", "origin": "user", "created_at": "t2", "revision_id": "E2", "semver": "1.1.0", "content_digest": "22222222bb"},
                {"kind": "evals", "origin": "user", "created_at": "t3", "revision_id": "E3", "semver": "1.0.1", "content_digest": "33333333cc"}
            ]
        })
    }

    #[test]
    fn read_text_rejects_devices_fifos_and_oversize_bytes() {
        let err = read_text(Path::new("/dev/zero")).unwrap_err();
        assert!(err.contains("不是普通文件"), "{err}");

        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let fifo_for_thread = fifo.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read_text(&fifo_for_thread));
        });
        let err = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("FIFO 读取卡住")
            .unwrap_err();
        assert!(err.contains("不是普通文件"), "{err}");

        let big = dir.path().join("big.md");
        std::fs::write(&big, vec![b'a'; agent24_skill::BODY_MAX_BYTES + 1]).unwrap();
        let err = read_text(&big).unwrap_err();
        assert!(err.contains("超过"), "{err}");

        let exact = dir.path().join("exact.md");
        std::fs::write(&exact, vec![b'a'; agent24_skill::BODY_MAX_BYTES]).unwrap();
        assert_eq!(
            read_text(&exact).unwrap().len(),
            agent24_skill::BODY_MAX_BYTES
        );

        let binary = dir.path().join("bin.md");
        std::fs::write(&binary, [0xff, 0xfe]).unwrap();
        let err = read_text(&binary).unwrap_err();
        assert!(err.contains("UTF-8"), "{err}");
    }

    #[test]
    fn list_row_shows_eight_chars_or_inactive_and_hides_other_origins() {
        let detail = json!({
            "pointer": {
                "origin": "imported",
                "body_revision_id": "abcdef012345",
                "evals_revision_id": "12345678abcd"
            },
            "text_consistency": {"consistent": 1, "total": 2}
        });
        assert_eq!(pointer_cell(Some(&detail), "imported"), "abcdef01/12345678");
        assert_eq!(consistency_cell(Some(&detail), "imported"), "1/2");
        assert_eq!(pointer_cell(Some(&detail), "user"), "未激活");
        assert_eq!(consistency_cell(Some(&detail), "user"), "—");
    }

    #[test]
    fn eval_failure_prints_the_code_and_the_case_summary() {
        let text = r#"{"error":{"code":"skill_eval_failed","message":"有失败用例未被接受","details":{"cases":[{"id":"abc","line":2,"judge":"contains","passed":false,"expect_digest":"deadbeef00"}],"failed":["abc"]}}}"#;
        let explained = explain_failure(text);
        assert!(explained.starts_with("skill_eval_failed:"));
        assert!(explained.contains("passed=false"));
        assert!(explained.contains("失败: --accept abc"));
        let two = r#"{"error":{"code":"skill_eval_failed","message":"有失败用例未被接受","details":{"failed":["a","b"]}}}"#;
        assert!(explain_failure(two).contains("失败: --accept a --accept b"));
        assert!(!explained.contains("secret expect"));
    }

    #[test]
    fn daemon_error_strips_osc_from_the_unknown_eval_field() {
        let key = "\u{1b}]0;pwned\u{7}\u{9b}";
        let text = serde_json::json!({
            "error": {
                "code": "invalid_request",
                "message": format!("unknown eval field {key}"),
                "details": {
                    "cases": [{
                        "id": key,
                        "line": 1,
                        "judge": "contains",
                        "passed": false,
                        "expect_digest": "abcd"
                    }],
                    "failed": [key]
                }
            }
        })
        .to_string();
        let explained = explain_failure(&text);
        assert!(explained.contains("unknown eval field ]0;pwned"));
        assert!(explained.contains("--accept ]0;pwned"));
        assert!(
            !explained.chars().any(|ch| ch.is_control() && ch != '\n'),
            "{explained:?}"
        );
        assert!(!explained.contains('\u{1b}'));
        assert!(!explained.contains('\u{7}'));
        assert!(!explained.contains('\u{9b}'));
    }

    #[test]
    fn show_text_does_not_invent_a_body() {
        let detail = json!({
            "skill_id": "weekly-report",
            "generation": 0,
            "pointer": null,
            "revisions": [{"revision_id": "rev", "kind": "body", "origin": "imported", "semver": "1.2.0", "content_digest": "abcdef0123"}],
            "activations": [],
            "text_consistency": null
        });
        let text = show_text(&detail);
        assert!(text.contains("未激活"));
        assert!(text.contains("rev"));
        assert!(!text.contains("hello world"));
    }
}
