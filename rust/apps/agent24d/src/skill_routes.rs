//! Skill import and user-directory scan (skill-loader SL-3).
//!
//! `POST /api/v1/skills` accepts JSON (`markdown`, optional `evals`). It does
//! not accept a path and does not open one. Import is always `origin=imported`
//! and goes through [`agent24_skill::parse_imported`].
//!
//! The scanner reads `<data_dir>/skills/<name>/` only, by `openat` from that
//! root. Directories use `O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC`. Files use
//! `O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC`. `fstat` must show a directory or a
//! regular file. Reads use `take(remaining + 1)` and the directory quota
//! counts bytes actually read. `evals/` is opened from the skill directory
//! handle. Caps: 64 skill directories, 8 directories and depth 2 inside one
//! skill, 32 regular files, 256 KiB. Validation, the manifest digest, and the
//! stored eval bytes all come from that one snapshot.
//!
//! Neither path moves the skill pointer. Activation is SL-4. Provider status
//! is SL-5 and is omitted here.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;

use agent24_skill::{SkillError, SkillManifest};
use agent24_store::{NewSkillRevision, SkillOrigin, SkillRevisionInsert, SkillRevisionKind, Store};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::server::{AppState, error_response};

const MAX_FILES: usize = 32;
const MAX_TOTAL_BYTES: u64 = 256 * 1024;
/// Direct children of `<data_dir>/skills`. Plan §4.3.
const MAX_SKILL_DIRS: usize = 64;
/// Dirents read past the file cap before a skill directory stops. A skill
/// that is exactly at 32 files still finishes. One far past it is not listed
/// to the end.
const ENTRY_READ_SLACK: usize = 8;
/// Directories opened under one skill, including the skill directory.
const MAX_WALK_DIRS: usize = 8;
/// Skill directory is depth 1. `evals/` is depth 2.
const MAX_DIR_DEPTH: usize = 2;
const REQUEST_MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportRequest {
    markdown: String,
    #[serde(default)]
    evals: Vec<ImportEvalFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportEvalFile {
    name: String,
    content: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScanRequest {}

#[derive(Debug, Serialize)]
pub(crate) struct InstallResponse {
    skill_id: String,
    origin: String,
    semver: String,
    body_revision_id: String,
    evals_revision_id: String,
    content_digest: String,
    ineffective: Vec<String>,
    inserted: bool,
    pointer: Option<PointerView>,
    generation: i64,
    /// Counts for the body and evals this call wrote or reused. Both sides
    /// are the bytes in hand, not a later pairing of unrelated rows.
    text_consistency: Option<ConsistencyView>,
}

#[derive(Debug, Serialize)]
struct PointerView {
    origin: String,
    body_revision_id: String,
    evals_revision_id: String,
    generation: i64,
}

#[derive(Debug, Serialize)]
struct SkillListResponse {
    skills: Vec<SkillListItem>,
}

#[derive(Debug, Serialize)]
struct SkillListItem {
    skill_id: String,
    origin: String,
    semver: String,
    generation: i64,
    active: bool,
}

#[derive(Debug, Serialize)]
struct SkillDetail {
    skill_id: String,
    pointer: Option<PointerView>,
    generation: i64,
    revisions: Vec<RevisionView>,
    /// Counts for the pointer's body and evals revisions. `None` when the
    /// skill has no pointer. v1 does not infer a pair. Provider status is
    /// SL-5 and is omitted.
    text_consistency: Option<ConsistencyView>,
}

#[derive(Debug, Serialize)]
struct RevisionView {
    revision_id: String,
    origin: String,
    semver: String,
    kind: String,
    content_digest: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct ConsistencyView {
    consistent: usize,
    total: usize,
}

#[derive(Debug, Serialize)]
pub struct ScanReport {
    pub origin: String,
    pub skills: Vec<InstallResponse>,
    pub errors: Vec<ScanError>,
}

#[derive(Debug, Serialize)]
pub struct ScanError {
    pub path: String,
    pub code: String,
    pub message: String,
}

struct StoredSkill {
    response: InstallResponse,
}

enum Fail {
    Http(StatusCode, &'static str, String),
}

pub async fn list_skills(State(state): State<AppState>) -> Response {
    match skill_list(&state.store).await {
        Ok(body) => Json(body).into_response(),
        Err(fail) => fail_response(&fail),
    }
}

pub async fn get_skill(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    match skill_detail(&state.store, &id).await {
        Ok(body) => Json(body).into_response(),
        Err(fail) => fail_response(&fail),
    }
}

pub async fn post_skill(State(state): State<AppState>, body: Body) -> Response {
    let bytes = match read_body(body).await {
        Ok(bytes) => bytes,
        Err(fail) => return fail_response(&fail),
    };
    let request: ImportRequest = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "请求体只接受 markdown 和 evals",
            );
        }
    };
    // The request has no path. Nothing below opens a caller-supplied path.
    let manifest = match agent24_skill::parse_imported(&request.markdown) {
        Ok(manifest) => manifest,
        Err(err) => return fail_response(&Fail::from_skill(err)),
    };
    let eval_text = match select_import_eval(&manifest, &request.evals) {
        Ok(text) => text,
        Err(err) => return fail_response(&Fail::from_skill(err)),
    };
    if let Err(err) = validate_eval(SkillOrigin::Imported, &manifest, &eval_text) {
        return fail_response(&err);
    }
    match store_pair(
        &state.store,
        SkillOrigin::Imported,
        &manifest,
        &request.markdown,
        &eval_text,
        manifest.ineffective.clone(),
    )
    .await
    {
        Ok(stored) => Json(stored.response).into_response(),
        Err(fail) => fail_response(&fail),
    }
}

pub async fn post_skill_scan(State(state): State<AppState>, body: Body) -> Response {
    let bytes = match read_body(body).await {
        Ok(bytes) => bytes,
        Err(fail) => return fail_response(&fail),
    };
    if !bytes.is_empty() {
        let parsed: Result<ScanRequest, _> = serde_json::from_slice(&bytes);
        if parsed.is_err() {
            return error_response(StatusCode::BAD_REQUEST, "invalid_request", "重扫不接受路径");
        }
    }
    Json(scan_installed_user_skills(&state.store).await).into_response()
}

/// Startup scan of `<data_dir>/skills`. A missing directory is an empty scan.
/// One bad skill does not fail the caller.
pub async fn scan_installed_user_skills(store: &Store) -> ScanReport {
    // Test seam so a startup scan can be held past the ready line.
    // `AGENT24_SKILL_SCAN_DELAY_MS` is not a LaunchAgent setting.
    pause_scan_for_tests().await;
    let Some(dir) = agent24_protocol::state_file::state_dir() else {
        return ScanReport::empty();
    };
    scan_root(store, &dir.join("skills")).await
}

async fn pause_scan_for_tests() {
    let Ok(raw) = std::env::var("AGENT24_SKILL_SCAN_DELAY_MS") else {
        return;
    };
    let Ok(ms) = raw.parse::<u64>() else {
        return;
    };
    if (1..=5_000).contains(&ms) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
}

/// Scan `skills_root` as `origin=user`. Does not create or modify the directory.
pub async fn scan_root(store: &Store, skills_root: &Path) -> ScanReport {
    let root = skills_root.to_path_buf();
    let prepared = match tokio::task::spawn_blocking(move || prepare_root(&root)).await {
        Ok(prepared) => prepared,
        Err(_) => {
            return ScanReport::failed(skills_root, "skill_manifest_invalid", "scan task failed");
        }
    };
    let mut report = ScanReport::empty();
    for item in prepared {
        match item {
            Prepared::Failed {
                path,
                code,
                message,
            } => {
                report.errors.push(ScanError {
                    path,
                    code,
                    message,
                });
            }
            Prepared::Loaded(loaded) => match install_loaded(store, &loaded).await {
                Ok(stored) => report.skills.push(stored.response),
                Err(fail) => {
                    let (code, message) = fail.code_message();
                    report.errors.push(ScanError {
                        path: loaded.dir_name,
                        code,
                        message,
                    });
                }
            },
        }
    }
    report
        .skills
        .sort_by(|left, right| left.skill_id.cmp(&right.skill_id));
    report
        .errors
        .sort_by(|left, right| left.path.cmp(&right.path));
    report
}

async fn install_loaded(store: &Store, loaded: &LoadedSkill) -> Result<StoredSkill, Fail> {
    let manifest =
        agent24_skill::parse_skill_markdown(&loaded.markdown).map_err(Fail::from_skill)?;
    if manifest.name != loaded.dir_name {
        return Err(Fail::from_skill(SkillError::ManifestInvalid {
            detail: "directory name does not match the skill name".to_owned(),
        }));
    }
    let (eval_text, consumed) = eval_from_snapshot(&manifest, &loaded.ingested)?;
    validate_eval(SkillOrigin::User, &manifest, &eval_text)?;
    let mut ineffective = manifest.ineffective.clone();
    ineffective.extend(loaded.skipped.iter().cloned());
    for name in loaded.ingested.keys() {
        if name != "SKILL.md" && Some(name.as_str()) != consumed.as_deref() {
            ineffective.push(name.clone());
        }
    }
    ineffective.sort();
    ineffective.dedup();
    store_pair(
        store,
        SkillOrigin::User,
        &manifest,
        &loaded.markdown,
        &eval_text,
        ineffective,
    )
    .await
}

fn eval_from_snapshot(
    manifest: &SkillManifest,
    ingested: &BTreeMap<String, String>,
) -> Result<(String, Option<String>), Fail> {
    if manifest.evals.is_empty() {
        return Ok((String::new(), None));
    }
    let key = constrained_eval_key(&manifest.evals).map_err(Fail::from_skill)?;
    let Some(text) = ingested.get(&key) else {
        return Err(Fail::from_skill(SkillError::EvalInvalid {
            detail: format!("evals file is not in the skill directory: {key}"),
        }));
    };
    Ok((text.clone(), Some(key)))
}

/// Reject a frontmatter eval path before any join onto the skill directory.
/// The accepted shape is one `evals/<file>.jsonl` segment. The bytes, if any,
/// are taken from the snapshot map, never from this string as a filesystem path.
fn constrained_eval_key(path: &str) -> Result<String, SkillError> {
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    let escapes = path.starts_with('/')
        || path.starts_with('\\')
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..");
    if escapes || parts.len() != 2 || parts[0] != "evals" || !parts[1].ends_with(".jsonl") {
        return Err(SkillError::EvalInvalid {
            detail: format!("evals path escapes the skill directory: {path}"),
        });
    }
    Ok(format!("evals/{}", parts[1]))
}

fn select_import_eval(
    manifest: &SkillManifest,
    evals: &[ImportEvalFile],
) -> Result<String, SkillError> {
    if manifest.evals.is_empty() {
        if evals.is_empty() {
            return Ok(String::new());
        }
        return Err(SkillError::EvalInvalid {
            detail: "evals were not declared".to_owned(),
        });
    }
    let key = constrained_eval_key(&manifest.evals)?;
    if evals.len() != 1 {
        return Err(SkillError::EvalInvalid {
            detail: "evals entry does not match the manifest path".to_owned(),
        });
    }
    let name = constrained_eval_key(&evals[0].name)?;
    if name != key {
        return Err(SkillError::EvalInvalid {
            detail: "evals entry does not match the manifest path".to_owned(),
        });
    }
    if evals[0].content.len() > agent24_skill::BODY_MAX_BYTES {
        return Err(SkillError::BodyTooLarge);
    }
    Ok(evals[0].content.clone())
}

fn validate_eval(
    origin: SkillOrigin,
    manifest: &SkillManifest,
    eval_text: &str,
) -> Result<(), Fail> {
    if manifest.evals.is_empty() {
        return Ok(());
    }
    if eval_text.len() > agent24_skill::BODY_MAX_BYTES {
        return Err(Fail::from_skill(SkillError::BodyTooLarge));
    }
    match agent24_skill::check_text_consistency(
        &manifest.evals,
        eval_text,
        &manifest.body,
        skill_origin(origin),
        &manifest.content_digest(),
    ) {
        Ok(_) | Err(SkillError::EvalEmpty) => Ok(()),
        Err(err) => Err(Fail::from_skill(err)),
    }
}

async fn store_pair(
    store: &Store,
    origin: SkillOrigin,
    manifest: &SkillManifest,
    markdown: &str,
    eval_text: &str,
    ineffective: Vec<String>,
) -> Result<StoredSkill, Fail> {
    let created_at = chrono::Utc::now().to_rfc3339();
    // Body rows store this parse's manifest digest. The store does not
    // recompute it; calling `content_digest` twice cannot check anything.
    let content_digest = manifest.content_digest();
    let body = NewSkillRevision {
        revision_id: uuid::Uuid::new_v4().to_string(),
        skill_id: manifest.name.clone(),
        origin,
        semver: manifest.version.clone(),
        kind: SkillRevisionKind::Body,
        content_digest: content_digest.clone(),
        content: markdown.as_bytes().to_vec(),
        created_at: created_at.clone(),
    };
    let evals = NewSkillRevision {
        revision_id: uuid::Uuid::new_v4().to_string(),
        skill_id: manifest.name.clone(),
        origin,
        semver: manifest.version.clone(),
        kind: SkillRevisionKind::Evals,
        content_digest: agent24_skill::sha256_hex(eval_text),
        content: eval_text.as_bytes().to_vec(),
        created_at,
    };
    let (body_row, evals_row) = store
        .install_skill_pair(&body, &evals)
        .await
        .map_err(Fail::from_store)?;
    let inserted = is_inserted(&body_row) || is_inserted(&evals_row);
    let pointer = store
        .get_skill_pointer(&manifest.name)
        .await
        .map_err(Fail::from_store)?;
    let generation = pointer.as_ref().map(|row| row.generation).unwrap_or(0);
    Ok(StoredSkill {
        response: InstallResponse {
            skill_id: manifest.name.clone(),
            origin: origin.as_str().to_owned(),
            semver: manifest.version.clone(),
            body_revision_id: revision_id_of(body_row),
            evals_revision_id: revision_id_of(evals_row),
            content_digest,
            ineffective,
            inserted,
            pointer: pointer.map(pointer_view),
            generation,
            text_consistency: consistency_view(origin, manifest, eval_text),
        },
    })
}

async fn skill_list(store: &Store) -> Result<SkillListResponse, Fail> {
    let rows = store
        .list_skill_revisions()
        .await
        .map_err(|_| Fail::internal("list skills"))?;
    let mut newest: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for row in &rows {
        ids.insert(row.skill_id.clone());
        if row.kind != SkillRevisionKind::Body {
            continue;
        }
        let key = (row.skill_id.clone(), row.origin.as_str().to_owned());
        // Stored semver of the newest body row. A version-only reuse does not
        // write the semver parsed on that later install.
        let replace = newest
            .get(&key)
            .is_none_or(|(_, created)| row.created_at.as_str() >= created.as_str());
        if replace {
            newest.insert(key, (row.semver.clone(), row.created_at.clone()));
        }
    }
    let mut skills = Vec::new();
    for skill_id in ids {
        let pointer = store
            .get_skill_pointer(&skill_id)
            .await
            .map_err(|_| Fail::internal("read pointer"))?;
        let origins: BTreeSet<String> = rows
            .iter()
            .filter(|row| row.skill_id == skill_id)
            .map(|row| row.origin.as_str().to_owned())
            .collect();
        for origin in origins {
            let semver = newest
                .get(&(skill_id.clone(), origin.clone()))
                .map(|(semver, _)| semver.clone())
                .unwrap_or_default();
            let active = pointer
                .as_ref()
                .is_some_and(|row| row.origin.as_str() == origin);
            let generation = if active {
                pointer.as_ref().map(|row| row.generation).unwrap_or(0)
            } else {
                0
            };
            skills.push(SkillListItem {
                skill_id: skill_id.clone(),
                origin,
                semver,
                generation,
                active,
            });
        }
    }
    skills.sort_by(|left, right| {
        (&left.skill_id, &left.origin).cmp(&(&right.skill_id, &right.origin))
    });
    Ok(SkillListResponse { skills })
}

async fn skill_detail(store: &Store, skill_id: &str) -> Result<SkillDetail, Fail> {
    let rows = store
        .list_skill_revisions()
        .await
        .map_err(|_| Fail::internal("list skills"))?;
    let mut mine: Vec<_> = rows
        .into_iter()
        .filter(|row| row.skill_id == skill_id)
        .collect();
    if mine.is_empty() {
        return Err(Fail::Http(
            StatusCode::NOT_FOUND,
            "not_found",
            "技能未安装".to_owned(),
        ));
    }
    mine.sort_by(|left, right| {
        (&left.created_at, &left.revision_id).cmp(&(&right.created_at, &right.revision_id))
    });
    let pointer = store
        .get_skill_pointer(skill_id)
        .await
        .map_err(|_| Fail::internal("read pointer"))?;
    let generation = pointer.as_ref().map(|row| row.generation).unwrap_or(0);
    let text_consistency = text_consistency(store, pointer.as_ref()).await?;
    let revisions = mine
        .iter()
        .map(|row| RevisionView {
            revision_id: row.revision_id.clone(),
            origin: row.origin.as_str().to_owned(),
            semver: row.semver.clone(),
            kind: row.kind.as_str().to_owned(),
            content_digest: row.content_digest.clone(),
            created_at: row.created_at.clone(),
        })
        .collect();
    Ok(SkillDetail {
        skill_id: skill_id.to_owned(),
        pointer: pointer.map(pointer_view),
        generation,
        revisions,
        text_consistency,
    })
}

fn consistency_view(
    origin: SkillOrigin,
    manifest: &SkillManifest,
    eval_text: &str,
) -> Option<ConsistencyView> {
    if manifest.evals.is_empty() || eval_text.is_empty() {
        return Some(ConsistencyView {
            consistent: 0,
            total: 0,
        });
    }
    match agent24_skill::check_text_consistency(
        &manifest.evals,
        eval_text,
        &manifest.body,
        skill_origin(origin),
        &manifest.content_digest(),
    ) {
        Ok(report) => {
            let (consistent, total) = report.consistent_counts();
            Some(ConsistencyView { consistent, total })
        }
        Err(SkillError::EvalEmpty) => Some(ConsistencyView {
            consistent: 0,
            total: 0,
        }),
        Err(_) => None,
    }
}

/// The activated pair only. No pointer means no consistency count.
async fn text_consistency(
    store: &Store,
    pointer: Option<&agent24_store::SkillPointer>,
) -> Result<Option<ConsistencyView>, Fail> {
    let Some(pointer) = pointer else {
        return Ok(None);
    };
    let body_row = store
        .get_skill_revision(&pointer.body_revision_id)
        .await
        .map_err(|_| Fail::internal("read body"))?;
    let eval_row = store
        .get_skill_revision(&pointer.evals_revision_id)
        .await
        .map_err(|_| Fail::internal("read evals"))?;
    let (Some(body_row), Some(eval_row)) = (body_row, eval_row) else {
        return Ok(None);
    };
    let Ok(markdown) = String::from_utf8(body_row.content) else {
        return Ok(None);
    };
    let Ok(eval_text) = String::from_utf8(eval_row.content) else {
        return Ok(None);
    };
    let Ok(manifest) = parse_for_origin(body_row.origin, &markdown) else {
        return Ok(None);
    };
    Ok(consistency_view(body_row.origin, &manifest, &eval_text))
}

fn parse_for_origin(origin: SkillOrigin, markdown: &str) -> Result<SkillManifest, SkillError> {
    match origin {
        SkillOrigin::User => agent24_skill::parse_skill_markdown(markdown),
        SkillOrigin::Imported => agent24_skill::parse_imported(markdown),
    }
}

fn skill_origin(origin: SkillOrigin) -> agent24_skill::SkillOrigin {
    match origin {
        SkillOrigin::User => agent24_skill::SkillOrigin::User,
        SkillOrigin::Imported => agent24_skill::SkillOrigin::Imported,
    }
}

fn pointer_view(pointer: agent24_store::SkillPointer) -> PointerView {
    PointerView {
        origin: pointer.origin.as_str().to_owned(),
        body_revision_id: pointer.body_revision_id,
        evals_revision_id: pointer.evals_revision_id,
        generation: pointer.generation,
    }
}

fn revision_id_of(row: SkillRevisionInsert) -> String {
    match row {
        SkillRevisionInsert::Inserted { revision_id }
        | SkillRevisionInsert::Existing { revision_id } => revision_id,
    }
}

fn is_inserted(row: &SkillRevisionInsert) -> bool {
    matches!(row, SkillRevisionInsert::Inserted { .. })
}

impl Fail {
    fn from_skill(err: SkillError) -> Self {
        let status = match err {
            SkillError::BodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            _ => StatusCode::BAD_REQUEST,
        };
        let message = match &err {
            SkillError::BodyTooLarge => "正文或评测超过 32 KiB".to_owned(),
            SkillError::ProactiveUnsupported => "v1 不支持主动技能".to_owned(),
            SkillError::ManifestInvalid { detail } => detail.clone(),
            SkillError::CapabilityUnknown { id } => format!("未知能力 {id}"),
            SkillError::EvalInvalid { detail } => detail.clone(),
            SkillError::EvalEmpty => "评测没有用例".to_owned(),
            SkillError::IdReserved => "技能 id 是保留名".to_owned(),
        };
        Self::Http(status, err.code(), message)
    }

    fn from_store(err: agent24_store::StoreError) -> Self {
        match err {
            agent24_store::StoreError::SkillRevisionConflict => Self::Http(
                StatusCode::CONFLICT,
                "skill_revision_conflict",
                "同版本内容不同，请升 version".to_owned(),
            ),
            other => {
                tracing::error!(error = %other, "skill store failed");
                Self::Http(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "skill store failed".to_owned(),
                )
            }
        }
    }

    fn code_message(&self) -> (String, String) {
        match self {
            Self::Http(_, code, message) => ((*code).to_owned(), message.clone()),
        }
    }

    fn internal(action: &str) -> Self {
        Self::Http(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            format!("skill store failed: {action}"),
        )
    }
}

fn fail_response(fail: &Fail) -> Response {
    match fail {
        Fail::Http(status, code, message) => error_response(*status, code, message),
    }
}

async fn read_body(body: Body) -> Result<Vec<u8>, Fail> {
    match axum::body::to_bytes(body, REQUEST_MAX_BYTES).await {
        Ok(bytes) => Ok(bytes.to_vec()),
        Err(_) => Err(Fail::Http(
            StatusCode::PAYLOAD_TOO_LARGE,
            "skill_body_too_large",
            "正文超过 32 KiB".to_owned(),
        )),
    }
}

impl ScanReport {
    fn empty() -> Self {
        Self {
            origin: "user".to_owned(),
            skills: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn failed(path: &Path, code: &str, message: &str) -> Self {
        Self {
            origin: "user".to_owned(),
            skills: Vec::new(),
            errors: vec![ScanError {
                path: path.display().to_string(),
                code: code.to_owned(),
                message: message.to_owned(),
            }],
        }
    }
}

struct LoadedSkill {
    dir_name: String,
    markdown: String,
    ingested: BTreeMap<String, String>,
    skipped: Vec<String>,
}

enum Prepared {
    Failed {
        path: String,
        code: String,
        message: String,
    },
    Loaded(LoadedSkill),
}

#[cfg(unix)]
fn dir_flags() -> rustix::fs::OFlags {
    rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC
}

#[cfg(unix)]
fn file_flags() -> rustix::fs::OFlags {
    rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC
}

#[cfg(unix)]
enum OpenDir {
    Dir(OwnedFd),
    NotDirectory,
    Symlink,
    Missing,
    Error(String),
}

#[cfg(unix)]
fn open_directory(parent: impl AsFd + Copy, path: impl rustix::path::Arg + Copy) -> OpenDir {
    match rustix::fs::openat(parent, path, dir_flags(), rustix::fs::Mode::empty()) {
        Ok(fd) => match file_type(&fd) {
            Ok(rustix::fs::FileType::Directory) => OpenDir::Dir(fd),
            Ok(_) => OpenDir::NotDirectory,
            Err(err) => OpenDir::Error(err),
        },
        Err(rustix::io::Errno::LOOP) | Err(rustix::io::Errno::MLINK) => OpenDir::Symlink,
        // macOS returns ENOTDIR, not ELOOP, for O_DIRECTORY|O_NOFOLLOW on a symlink.
        Err(rustix::io::Errno::NOTDIR) if nofollow_symlink(parent, path) => OpenDir::Symlink,
        Err(rustix::io::Errno::NOTDIR) => OpenDir::NotDirectory,
        Err(rustix::io::Errno::NOENT) => OpenDir::Missing,
        Err(err) => OpenDir::Error(io_message(err)),
    }
}

/// `AT_SYMLINK_NOFOLLOW` on the already-open parent. Does not open the target.
#[cfg(unix)]
fn nofollow_symlink(parent: impl AsFd, path: impl rustix::path::Arg) -> bool {
    rustix::fs::statat(parent, path, rustix::fs::AtFlags::SYMLINK_NOFOLLOW).is_ok_and(|stat| {
        rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::Symlink
    })
}

#[cfg(unix)]
fn file_type(fd: impl AsFd) -> Result<rustix::fs::FileType, String> {
    let stat = rustix::fs::fstat(fd).map_err(io_message)?;
    Ok(rustix::fs::FileType::from_raw_mode(stat.st_mode))
}

#[cfg(unix)]
fn io_message(err: rustix::io::Errno) -> String {
    std::io::Error::from(err).to_string()
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Walk {
    Next,
    Stop,
}

/// Read `readdir` one entry at a time. `visit` returning [`Walk::Stop`] ends
/// the listing. Names are not collected and then sorted.
#[cfg(unix)]
fn for_each_entry(
    dir: &OwnedFd,
    mut visit: impl FnMut(&str) -> Result<Walk, String>,
) -> Result<(), String> {
    let listed = rustix::fs::Dir::read_from(dir).map_err(io_message)?;
    for entry in listed {
        let entry = entry.map_err(io_message)?;
        let name = entry
            .file_name()
            .to_str()
            .map_err(|_| "skill directory entry is not utf-8".to_owned())?
            .to_owned();
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        if name.contains('/') || name.contains('\\') {
            return Err(format!("skill directory entry escapes: {name}"));
        }
        if visit(&name)? == Walk::Stop {
            break;
        }
    }
    Ok(())
}

fn prepare_root(root: &Path) -> Vec<Prepared> {
    #[cfg(not(unix))]
    {
        let _ = root;
        return vec![failed(
            root.display().to_string(),
            "skill_manifest_invalid",
            "skill scan requires unix",
        )];
    }
    #[cfg(unix)]
    {
        let root_fd = match open_directory(rustix::fs::CWD, root) {
            OpenDir::Dir(fd) => fd,
            OpenDir::NotDirectory => {
                return vec![failed(
                    root.display().to_string(),
                    "skill_manifest_invalid",
                    "skills root is not a directory",
                )];
            }
            OpenDir::Symlink => {
                return vec![failed(
                    root.display().to_string(),
                    "skill_manifest_invalid",
                    "symlink rejected",
                )];
            }
            OpenDir::Missing => return Vec::new(),
            OpenDir::Error(message) => {
                return vec![failed(
                    root.display().to_string(),
                    "skill_manifest_invalid",
                    &message,
                )];
            }
        };
        let mut prepared = Vec::new();
        let mut skill_dirs = 0usize;
        let mut capped = false;
        if let Err(message) = for_each_entry(&root_fd, |name| {
            match open_directory(&root_fd, name) {
                OpenDir::Dir(fd) => {
                    skill_dirs += 1;
                    if skill_dirs > MAX_SKILL_DIRS {
                        capped = true;
                        return Ok(Walk::Stop);
                    }
                    prepared.push(prepare_skill(name, fd));
                }
                OpenDir::NotDirectory => {}
                OpenDir::Symlink => {
                    prepared.push(failed(name, "skill_manifest_invalid", "symlink rejected"));
                }
                OpenDir::Missing => {
                    prepared.push(failed(
                        name,
                        "skill_manifest_invalid",
                        "skill directory is missing",
                    ));
                }
                OpenDir::Error(message) => {
                    prepared.push(failed(name, "skill_manifest_invalid", &message));
                }
            }
            Ok(Walk::Next)
        }) {
            return vec![failed(
                root.display().to_string(),
                "skill_manifest_invalid",
                &message,
            )];
        }
        if capped {
            prepared.push(failed(
                ".",
                "skill_manifest_invalid",
                "skill directory count exceeds 64",
            ));
        }
        prepared
    }
}

#[cfg(unix)]
fn prepare_skill(name: &str, dir: OwnedFd) -> Prepared {
    let mut ingested = BTreeMap::new();
    let mut skipped = Vec::new();
    let mut quota = Quota::default();
    if let Err(message) = walk_skill(&dir, "", 1, &mut quota, &mut ingested, &mut skipped) {
        return failed(name, "skill_manifest_invalid", &message);
    }
    let Some(markdown) = ingested.get("SKILL.md").cloned() else {
        return failed(name, "skill_manifest_invalid", "missing SKILL.md");
    };
    Prepared::Loaded(LoadedSkill {
        dir_name: name.to_owned(),
        markdown,
        ingested,
        skipped,
    })
}

#[cfg(unix)]
#[derive(Default)]
struct Quota {
    dirs: usize,
    files: usize,
    bytes: u64,
}

#[cfg(unix)]
fn walk_skill(
    dir: &OwnedFd,
    rel_dir: &str,
    depth: usize,
    quota: &mut Quota,
    ingested: &mut BTreeMap<String, String>,
    skipped: &mut Vec<String>,
) -> Result<(), String> {
    if depth > MAX_DIR_DEPTH {
        return Err("skill directory exceeds depth 2".to_owned());
    }
    quota.dirs += 1;
    if quota.dirs > MAX_WALK_DIRS {
        return Err("skill directory exceeds 8 directories".to_owned());
    }
    let mut entries = 0usize;
    for_each_entry(dir, |name| {
        entries += 1;
        if entries > MAX_FILES + ENTRY_READ_SLACK {
            return Err("skill directory exceeds 32 files".to_owned());
        }
        let rel = if rel_dir.is_empty() {
            name.to_owned()
        } else {
            format!("{rel_dir}/{name}")
        };
        match open_directory(dir, name) {
            OpenDir::Dir(child) => {
                walk_skill(&child, &rel, depth + 1, quota, ingested, skipped)?;
            }
            OpenDir::Symlink => return Err(format!("symlink rejected: {rel}")),
            OpenDir::NotDirectory => read_regular(dir, name, &rel, quota, ingested, skipped)?,
            OpenDir::Missing => return Err(format!("skill directory entry is missing: {rel}")),
            OpenDir::Error(message) => return Err(message),
        }
        Ok(Walk::Next)
    })
}

#[cfg(unix)]
fn read_regular(
    dir: &OwnedFd,
    name: &str,
    rel: &str,
    quota: &mut Quota,
    ingested: &mut BTreeMap<String, String>,
    skipped: &mut Vec<String>,
) -> Result<(), String> {
    let fd =
        rustix::fs::openat(dir, name, file_flags(), rustix::fs::Mode::empty()).map_err(|err| {
            match err {
                rustix::io::Errno::LOOP | rustix::io::Errno::MLINK => {
                    format!("symlink rejected: {rel}")
                }
                other => io_message(other),
            }
        })?;
    if file_type(&fd)? != rustix::fs::FileType::RegularFile {
        return Err(format!("not a regular file: {rel}"));
    }
    let remaining = MAX_TOTAL_BYTES.saturating_sub(quota.bytes);
    let buf = read_capped(fd, remaining)?;
    quota.bytes += buf.len() as u64;
    quota.files += 1;
    if quota.files > MAX_FILES {
        return Err("skill directory exceeds 32 files".to_owned());
    }
    if quota.bytes > MAX_TOTAL_BYTES {
        return Err("skill directory exceeds 256 KiB".to_owned());
    }
    if is_ingested(rel) {
        let text = String::from_utf8(buf).map_err(|_| "skill file is not utf-8".to_owned())?;
        ingested.insert(rel.to_owned(), text);
    } else {
        skipped.push(rel.to_owned());
    }
    Ok(())
}

/// Read at most `remaining + 1` bytes. More than `remaining` means the file,
/// including bytes appended after the scan opened it, is over the quota.
#[cfg(unix)]
fn read_capped(fd: OwnedFd, remaining: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut file = std::fs::File::from(fd);
    let allowance = remaining.saturating_add(1);
    let mut limited = (&mut file).take(allowance);
    let mut buf = Vec::new();
    limited
        .read_to_end(&mut buf)
        .map_err(|err| err.to_string())?;
    if buf.len() as u64 > remaining {
        return Err("skill directory exceeds 256 KiB".to_owned());
    }
    Ok(buf)
}

#[cfg(unix)]
fn is_ingested(rel: &str) -> bool {
    if rel == "SKILL.md" {
        return true;
    }
    let mut parts = rel.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("evals"), Some(file), None) => file.ends_with(".jsonl") && !file.is_empty(),
        _ => false,
    }
}

fn failed(path: impl Into<String>, code: &str, message: &str) -> Prepared {
    Prepared::Failed {
        path: path.into(),
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::server::build_router_with_modules;

    fn imported(body: &str) -> String {
        format!("---\nname: weekly-report\ndescription: 周报\n---\n{body}")
    }

    fn user_md(evals: &str, body: &str) -> String {
        format!(
            "---\nname: weekly-report\nversion: 1.2.0\ndescription: 周报\nrequires: []\nevals: {evals}\n---\n{body}"
        )
    }

    async fn call(
        state: &AppState,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", "Bearer testtoken")
            .header("content-type", "application/json")
            .body(Body::from(body.unwrap_or("").to_owned()))
            .unwrap();
        let response = build_router_with_modules(state.clone(), Router::new())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, value)
    }

    async fn stored_text(store: &Store) -> String {
        let mut out = String::new();
        for row in store.list_skill_revisions().await.unwrap() {
            let revision = store
                .get_skill_revision(&row.revision_id)
                .await
                .unwrap()
                .unwrap();
            out.push_str(&String::from_utf8_lossy(&revision.content));
            out.push('\n');
        }
        out
    }

    #[tokio::test]
    async fn post_rejects_a_path_and_does_not_open_it() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret.md");
        std::fs::write(
            &secret,
            "---\nname: path-leak\ndescription: 周报\n---\nPATHLEAKTOKEN\n",
        )
        .unwrap();
        let fifo = dir.path().join("pipe");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let state = crate::server::tests::state().await;
        let body = json!({
            "markdown": imported("hello\n"),
            "path": secret,
        })
        .to_string();
        let (status, value) = call(&state, "POST", "/api/v1/skills", Some(&body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "invalid_request");
        assert!(!value.to_string().contains("PATHLEAKTOKEN"));
        assert!(!stored_text(&state.store).await.contains("PATHLEAKTOKEN"));
        assert!(!stored_text(&state.store).await.contains("path-leak"));

        let body = json!({
            "markdown": imported("hello\n"),
            "path": fifo,
        })
        .to_string();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            call(&state, "POST", "/api/v1/skills", Some(&body)),
        )
        .await;
        let (status, value) = result.expect("POST opened a path");
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "invalid_request");
    }

    #[tokio::test]
    async fn import_is_idempotent_and_leaves_the_pointer_empty() {
        let state = crate::server::tests::state().await;
        let markdown = imported("hello\n");
        let (status, first) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": markdown }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        assert_eq!(first["origin"], "imported");
        assert_eq!(first["inserted"], true);
        assert!(first["pointer"].is_null());
        assert_eq!(first["generation"], 0);
        assert_eq!(first["semver"], "0.1.0");
        assert_eq!(first["text_consistency"]["consistent"], 0);
        assert_eq!(first["text_consistency"]["total"], 0);
        assert!(first["ineffective"].as_array().unwrap().is_empty());
        let (status, second) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": markdown }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{second}");
        assert_eq!(second["inserted"], false);
        assert_eq!(second["body_revision_id"], first["body_revision_id"]);
        assert_eq!(second["evals_revision_id"], first["evals_revision_id"]);
        assert_eq!(state.store.list_skill_revisions().await.unwrap().len(), 2);
        assert!(
            state
                .store
                .get_skill_pointer("weekly-report")
                .await
                .unwrap()
                .is_none()
        );
        let (status, list) = call(&state, "GET", "/api/v1/skills", None).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        assert!(!list.to_string().contains("hello"));
        let (status, detail) = call(&state, "GET", "/api/v1/skills/weekly-report", None).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert!(!detail.to_string().contains("hello"));
        assert_eq!(detail["generation"], 0);
        assert!(detail["pointer"].is_null());
        assert!(detail["text_consistency"].is_null(), "{detail}");
        assert!(detail.get("providers").is_none());
    }

    #[tokio::test]
    async fn ineffective_list_comes_from_this_parse() {
        let state = crate::server::tests::state().await;
        let first_md = "---\nname: weekly-report\ndescription: 周报\nalpha: 1\nallowed-tools: [shell]\n---\nhello\n";
        let (status, first) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": first_md }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        assert_eq!(first["ineffective"], json!(["allowed-tools", "alpha"]));
        let second_md = "---\nname: weekly-report\ndescription: 周报\nbeta: 1\n---\nhello\n";
        let (status, second) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": second_md }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{second}");
        assert_eq!(second["ineffective"], json!(["beta"]));
        assert_eq!(second["inserted"], false);
        assert_eq!(second["body_revision_id"], first["body_revision_id"]);
        let stored = stored_text(&state.store).await;
        assert!(stored.contains("alpha"), "{stored}");
        assert!(!stored.contains("beta"), "{stored}");
    }

    #[tokio::test]
    async fn same_semver_and_different_bytes_conflict_and_keep_the_first_row() {
        let state = crate::server::tests::state().await;
        let first_md = "---\nname: weekly-report\nversion: 1.0.0\ndescription: 周报\n---\nhello\n";
        let (status, first) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": first_md }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let second_md =
            "---\nname: weekly-report\nversion: 1.0.0\ndescription: 周报\n---\nhello world\n";
        let (status, second) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": second_md }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{second}");
        assert_eq!(second["error"]["code"], "skill_revision_conflict");
        assert_eq!(state.store.list_skill_revisions().await.unwrap().len(), 2);
        let stored = stored_text(&state.store).await;
        assert!(stored.contains("hello\n"), "{stored}");
        assert!(!stored.contains("hello world"), "{stored}");
        assert!(
            state
                .store
                .get_skill_pointer("weekly-report")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn oversized_body_unknown_capability_and_proactive_stay_out_of_the_store() {
        let state = crate::server::tests::state().await;
        let big = format!(
            "{}{}",
            imported(""),
            "x".repeat(agent24_skill::BODY_MAX_BYTES)
        );
        let (status, value) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": big }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{value}");
        assert_eq!(value["error"]["code"], "skill_body_too_large");
        let unknown = "---\nname: weekly-report\ndescription: 周报\nrequires: [not-a-capability]\n---\nhello\n";
        let (status, value) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": unknown }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "skill_capability_unknown");
        let proactive =
            "---\nname: weekly-report\ndescription: 周报\nmode: proactive\n---\nhello\n";
        let (status, value) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": proactive }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "skill_proactive_unsupported");
        assert!(state.store.list_skill_revisions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn import_does_not_become_a_user_skill_and_a_scan_does_not_write() {
        let state = crate::server::tests::state().await;
        let (status, _) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": imported("hello\n") }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let root = tempfile::tempdir().unwrap();
        let skills = root.path().join("skills");
        std::fs::create_dir(&skills).unwrap();
        let before = std::fs::read_dir(&skills).unwrap().count();
        let report = scan_root(&state.store, &skills).await;
        assert!(report.skills.is_empty(), "{report:?}");
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(std::fs::read_dir(&skills).unwrap().count(), before);
        let origins: Vec<_> = state
            .store
            .list_skill_revisions()
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.origin.as_str().to_owned())
            .collect();
        assert!(origins.iter().all(|origin| origin == "imported"));
    }

    #[tokio::test]
    async fn user_scan_rejects_symlink_escape_fifo_and_limits() {
        let state = crate::server::tests::state().await;
        let secret = "SECRETTOKEN1827";
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside.txt");
        std::fs::write(
            &outside,
            format!("{{\"input\":\"x\",\"expect\":\"{secret}\",\"judge\":\"contains\"}}\n"),
        )
        .unwrap();
        let skills = root.path().join("skills");
        std::fs::create_dir(&skills).unwrap();

        let escaped = skills.join("weekly-report");
        std::fs::create_dir(&escaped).unwrap();
        std::fs::write(
            escaped.join("SKILL.md"),
            user_md("../../outside.txt", "hello\n"),
        )
        .unwrap();
        let report = scan_root(&state.store, &skills).await;
        assert!(report.skills.is_empty(), "{report:?}");
        assert!(
            report.errors.iter().any(|err| {
                err.code == "skill_eval_invalid"
                    && (err.message.contains("escapes") || err.message.contains(".."))
            }),
            "{report:?}"
        );
        assert!(!stored_text(&state.store).await.contains(secret));

        std::fs::remove_dir_all(&escaped).unwrap();
        std::fs::create_dir_all(escaped.join("evals")).unwrap();
        let outside_skill = root.path().join("linked.md");
        std::fs::write(&outside_skill, user_md("[]", &format!("{secret}\n"))).unwrap();
        std::os::unix::fs::symlink(&outside_skill, escaped.join("SKILL.md")).unwrap();
        let report = scan_root(&state.store, &skills).await;
        assert!(
            report
                .errors
                .iter()
                .any(|err| err.message.contains("symlink")),
            "{report:?}"
        );
        assert!(!stored_text(&state.store).await.contains(secret));

        std::fs::remove_dir_all(&escaped).unwrap();
        std::fs::create_dir(&escaped).unwrap();
        std::fs::write(escaped.join("SKILL.md"), user_md("[]", "hello\n")).unwrap();
        let fifo = escaped.join("pipe");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let scanned =
            tokio::time::timeout(Duration::from_secs(2), scan_root(&state.store, &skills))
                .await
                .expect("scan opened a fifo");
        assert!(
            scanned
                .errors
                .iter()
                .any(|err| err.message.contains("not a regular file")),
            "{scanned:?}"
        );
        assert!(state.store.list_skill_revisions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn user_scan_enforces_file_and_byte_caps_and_keeps_a_sibling() {
        let state = crate::server::tests::state().await;
        let root = tempfile::tempdir().unwrap();
        let skills = root.path().join("skills");
        let crowded = skills.join("weekly-report");
        std::fs::create_dir_all(&crowded).unwrap();
        std::fs::write(crowded.join("SKILL.md"), user_md("[]", "hello\n")).unwrap();
        for index in 0..32 {
            std::fs::write(crowded.join(format!("extra-{index}.txt")), b"x").unwrap();
        }
        let report = scan_root(&state.store, &skills).await;
        assert!(report.skills.is_empty(), "{report:?}");
        assert!(
            report.errors.iter().any(|err| err.message.contains("32")),
            "{report:?}"
        );

        std::fs::remove_dir_all(&crowded).unwrap();
        std::fs::create_dir(&crowded).unwrap();
        std::fs::write(crowded.join("SKILL.md"), user_md("[]", "hello\n")).unwrap();
        std::fs::write(crowded.join("pad.bin"), vec![b'a'; (256 * 1024) + 1]).unwrap();
        let report = scan_root(&state.store, &skills).await;
        assert!(
            report.errors.iter().any(|err| err.message.contains("256")),
            "{report:?}"
        );
        assert!(state.store.list_skill_revisions().await.unwrap().is_empty());

        std::fs::remove_dir_all(&crowded).unwrap();
        let good = skills.join("weekly-report");
        std::fs::create_dir_all(good.join("evals")).unwrap();
        let eval_line = "{\"input\":\"x\",\"expect\":\"hello\",\"judge\":\"contains\"}\n";
        std::fs::write(
            good.join("SKILL.md"),
            user_md("evals/check.jsonl", "hello\n"),
        )
        .unwrap();
        std::fs::write(good.join("evals").join("check.jsonl"), eval_line).unwrap();
        std::fs::write(good.join("notes.txt"), "NOTEONLY").unwrap();
        let bad = skills.join("other-name");
        std::fs::create_dir(&bad).unwrap();
        std::fs::write(bad.join("SKILL.md"), user_md("[]", "nope\n")).unwrap();
        std::fs::write(skills.join("README.txt"), "STRAY").unwrap();
        let before = walk_names(&skills);
        let report = scan_root(&state.store, &skills).await;
        assert_eq!(walk_names(&skills), before);
        assert_eq!(report.skills.len(), 1, "{report:?}");
        assert_eq!(report.skills[0].origin, "user");
        assert_eq!(report.skills[0].skill_id, "weekly-report");
        assert!(report.skills[0].pointer.is_none());
        let counts = report.skills[0].text_consistency.as_ref().unwrap();
        assert_eq!(counts.consistent, 1);
        assert_eq!(counts.total, 1);
        assert!(
            report.skills[0]
                .ineffective
                .iter()
                .any(|name| name == "notes.txt")
        );
        assert!(
            report.errors.iter().any(|err| err.path == "other-name"),
            "{report:?}"
        );
        let stored = stored_text(&state.store).await;
        assert!(!stored.contains("NOTEONLY"), "{stored}");
        assert!(!stored.contains("STRAY"), "{stored}");
        assert!(stored.contains("hello"), "{stored}");
        let (status, detail) = call(&state, "GET", "/api/v1/skills/weekly-report", None).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert!(detail["text_consistency"].is_null(), "{detail}");
        assert!(!detail.to_string().contains("NOTEONLY"));
    }

    #[tokio::test]
    async fn user_scan_rejects_a_symlinked_skill_dir_and_evals_dir() {
        let state = crate::server::tests::state().await;
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real-skill");
        std::fs::create_dir_all(real.join("evals")).unwrap();
        std::fs::write(
            real.join("SKILL.md"),
            user_md("evals/check.jsonl", "hello\n"),
        )
        .unwrap();
        std::fs::write(
            real.join("evals").join("check.jsonl"),
            "{\"input\":\"x\",\"expect\":\"hello\",\"judge\":\"contains\"}\n",
        )
        .unwrap();
        let skills = root.path().join("skills");
        std::fs::create_dir(&skills).unwrap();
        std::os::unix::fs::symlink(&real, skills.join("weekly-report")).unwrap();
        let report = scan_root(&state.store, &skills).await;
        assert!(report.skills.is_empty(), "{report:?}");
        assert!(
            report
                .errors
                .iter()
                .any(|err| err.message.contains("symlink")),
            "{report:?}"
        );
        assert!(state.store.list_skill_revisions().await.unwrap().is_empty());

        std::fs::remove_file(skills.join("weekly-report")).unwrap();
        let skill = skills.join("weekly-report");
        std::fs::create_dir(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            user_md("evals/check.jsonl", "hello\n"),
        )
        .unwrap();
        let outside = root.path().join("outside-evals");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(
            outside.join("check.jsonl"),
            "{\"input\":\"x\",\"expect\":\"SECRETTOKEN1827\",\"judge\":\"contains\"}\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, skill.join("evals")).unwrap();
        let report = scan_root(&state.store, &skills).await;
        assert!(report.skills.is_empty(), "{report:?}");
        assert!(
            report
                .errors
                .iter()
                .any(|err| err.message.contains("symlink")),
            "{report:?}"
        );
        assert!(!stored_text(&state.store).await.contains("SECRETTOKEN1827"));
        assert!(state.store.list_skill_revisions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn user_scan_rejects_a_file_that_grows_past_the_cap() {
        let state = crate::server::tests::state().await;
        let root = tempfile::tempdir().unwrap();
        let skills = root.path().join("skills");
        let skill = skills.join("weekly-report");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), user_md("[]", "hello\n")).unwrap();
        let pad = skill.join("pad.bin");
        std::fs::write(&pad, b"x").unwrap();
        let mut grown = std::fs::OpenOptions::new().append(true).open(&pad).unwrap();
        std::io::Write::write_all(&mut grown, &vec![b'Z'; (256 * 1024) + 1]).unwrap();
        drop(grown);
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let flag = running.clone();
        let pad_for_writer = pad.clone();
        let writer = std::thread::spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(pad_for_writer)
                .unwrap();
            let chunk = vec![b'Y'; 4096];
            while flag.load(std::sync::atomic::Ordering::Relaxed) {
                if std::io::Write::write_all(&mut file, &chunk).is_err() {
                    break;
                }
            }
        });
        let scanned =
            tokio::time::timeout(Duration::from_secs(2), scan_root(&state.store, &skills))
                .await
                .expect("scan blocked on a growing file");
        running.store(false, std::sync::atomic::Ordering::Relaxed);
        writer.join().unwrap();
        assert!(scanned.skills.is_empty(), "{scanned:?}");
        assert!(
            scanned.errors.iter().any(|err| err.message.contains("256")),
            "{scanned:?}"
        );
        assert!(state.store.list_skill_revisions().await.unwrap().is_empty());
        assert!(!stored_text(&state.store).await.contains("ZZZZ"));
    }

    #[tokio::test]
    async fn user_scan_rejects_a_directory_deeper_than_evals() {
        let state = crate::server::tests::state().await;
        let root = tempfile::tempdir().unwrap();
        let skill = root.path().join("skills").join("weekly-report");
        std::fs::create_dir_all(skill.join("evals").join("nested")).unwrap();
        std::fs::write(skill.join("SKILL.md"), user_md("[]", "hello\n")).unwrap();
        std::fs::write(skill.join("evals").join("nested").join("x.txt"), b"x").unwrap();
        let report = scan_root(&state.store, &root.path().join("skills")).await;
        assert!(report.skills.is_empty(), "{report:?}");
        assert!(
            report
                .errors
                .iter()
                .any(|err| err.message.contains("depth")),
            "{report:?}"
        );
        assert!(state.store.list_skill_revisions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn version_only_edit_reuses_the_body_and_an_upgrade_keeps_text_consistency() {
        let state = crate::server::tests::state().await;
        let v1 = "---\nname: weekly-report\nversion: 1.0.0\ndescription: 周报\n---\nhello\n";
        let v2 = "---\nname: weekly-report\nversion: 2.0.0\ndescription: 周报\n---\nhello\n";
        let (status, first) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": v1 }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let quoted =
            "---\nname: weekly-report\nversion: 1.0.0\ndescription: \"周报\"\n---\nhello\n";
        let (status, normalized) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": quoted }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{normalized}");
        assert_eq!(normalized["inserted"], false);
        assert_eq!(normalized["body_revision_id"], first["body_revision_id"]);
        let (status, second) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": v2 }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{second}");
        assert_eq!(second["semver"], "2.0.0");
        assert_eq!(second["inserted"], false);
        assert_eq!(second["body_revision_id"], first["body_revision_id"]);
        let (status, list) = call(&state, "GET", "/api/v1/skills", None).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let listed = list["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["skill_id"] == "weekly-report")
            .unwrap();
        assert_eq!(listed["semver"], "1.0.0", "{listed}");
        let stored = stored_text(&state.store).await;
        assert!(stored.contains("version: 1.0.0"), "{stored}");
        assert!(!stored.contains("version: 2.0.0"), "{stored}");

        let eval = "{\"input\":\"x\",\"expect\":\"hello\",\"judge\":\"contains\"}\n";
        let body_v1 = "---\nname: weekly-notes\nversion: 1.0.0\ndescription: 周报\nevals: evals/check.jsonl\n---\nhello\n";
        let body_v2 = "---\nname: weekly-notes\nversion: 2.0.0\ndescription: 周报\nevals: evals/check.jsonl\n---\ngoodbye\n";
        let (status, installed) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": body_v1, "evals": [{ "name": "evals/check.jsonl", "content": eval }] }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{installed}");
        let (status, upgraded) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": body_v2, "evals": [{ "name": "evals/check.jsonl", "content": eval }] }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{upgraded}");
        assert_eq!(upgraded["semver"], "2.0.0");
        assert_ne!(upgraded["body_revision_id"], installed["body_revision_id"]);
        assert_eq!(
            upgraded["evals_revision_id"],
            installed["evals_revision_id"]
        );
        assert_eq!(upgraded["text_consistency"]["total"], 1);
        assert_eq!(upgraded["text_consistency"]["consistent"], 0);
        let (status, detail) = call(&state, "GET", "/api/v1/skills/weekly-notes", None).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert!(detail["pointer"].is_null(), "{detail}");
        assert!(detail["text_consistency"].is_null(), "{detail}");

        let again = "---\nname: weekly-notes\nversion: 2.0.0\ndescription: 周报\nevals: evals/check.jsonl\n---\nhello\n";
        let (status, conflict) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": again, "evals": [{ "name": "evals/check.jsonl", "content": eval }] }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
        assert_eq!(conflict["error"]["code"], "skill_revision_conflict");
    }

    #[tokio::test]
    async fn interleaved_installs_do_not_invent_a_pair_on_the_detail() {
        let state = crate::server::tests::state().await;
        let case = |expect: &str| {
            format!("{{\"input\":\"x\",\"expect\":\"{expect}\",\"judge\":\"contains\"}}\n")
        };
        let install = |version: &str, body: &str, evals: &str| {
            let markdown = format!(
                "---\nname: weekly-report\nversion: {version}\ndescription: 周报\nevals: evals/check.jsonl\n---\n{body}"
            );
            json!({
                "markdown": markdown,
                "evals": [{ "name": "evals/check.jsonl", "content": evals }],
            })
            .to_string()
        };
        let first_body = install("1.0.0", "alpha-token\n", &case("alpha-token"));
        let (status, first) = call(&state, "POST", "/api/v1/skills", Some(&first_body)).await;
        assert_eq!(status, StatusCode::OK, "{first}");
        assert_eq!(first["text_consistency"]["consistent"], 1);
        assert_eq!(first["text_consistency"]["total"], 1);
        let second_evals = format!("{}{}", case("beta-token"), case("only-in-e2"));
        let second_body = install("2.0.0", "beta-token\n", &second_evals);
        let (status, second) = call(&state, "POST", "/api/v1/skills", Some(&second_body)).await;
        assert_eq!(status, StatusCode::OK, "{second}");
        assert_eq!(second["text_consistency"]["consistent"], 1);
        assert_eq!(second["text_consistency"]["total"], 2);
        let third_body = install("3.0.0", "alpha-token\n", &case("beta-token"));
        let (status, third) = call(&state, "POST", "/api/v1/skills", Some(&third_body)).await;
        assert_eq!(status, StatusCode::OK, "{third}");
        assert_eq!(third["body_revision_id"], first["body_revision_id"]);
        assert_ne!(third["evals_revision_id"], first["evals_revision_id"]);
        assert_ne!(third["evals_revision_id"], second["evals_revision_id"]);
        // (A, E3) is inconsistent. (B, E3) would have been consistent.
        assert_eq!(third["text_consistency"]["consistent"], 0, "{third}");
        assert_eq!(third["text_consistency"]["total"], 1, "{third}");
        let (status, detail) = call(&state, "GET", "/api/v1/skills/weekly-report", None).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert!(detail["pointer"].is_null(), "{detail}");
        assert!(detail["text_consistency"].is_null(), "{detail}");

        let outcome = state
            .store
            .activate_skill(&agent24_store::NewSkillActivation {
                activation_id: "act-pair".to_owned(),
                skill_id: "weekly-report".to_owned(),
                expected_generation: 0,
                body_revision_id: third["body_revision_id"].as_str().unwrap().to_owned(),
                evals_revision_id: third["evals_revision_id"].as_str().unwrap().to_owned(),
                binding: agent24_store::SkillEvalBinding {
                    result_id: "result-pair".to_owned(),
                    segment_digest: "segment".to_owned(),
                    renderer_version: "skill-render-1".to_owned(),
                    cases_json: r#"[{"id":"c1","passed":true}]"#.to_owned(),
                    accepted_json: "[]".to_owned(),
                },
                created_at: "2026-11-12T00:00:03Z".to_owned(),
                actor: "host".to_owned(),
            })
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            agent24_store::SkillActivationOutcome::Activated { .. }
        ));
        let (status, pointed) = call(&state, "GET", "/api/v1/skills/weekly-report", None).await;
        assert_eq!(status, StatusCode::OK, "{pointed}");
        assert_eq!(
            pointed["pointer"]["body_revision_id"],
            third["body_revision_id"]
        );
        assert_eq!(
            pointed["pointer"]["evals_revision_id"],
            third["evals_revision_id"]
        );
        assert_eq!(pointed["text_consistency"]["consistent"], 0, "{pointed}");
        assert_eq!(pointed["text_consistency"]["total"], 1, "{pointed}");
    }

    #[tokio::test]
    async fn user_scan_stops_at_64_skill_directories_with_one_summary_error() {
        let state = crate::server::tests::state().await;
        let root = tempfile::tempdir().unwrap();
        let skills = root.path().join("skills");
        std::fs::create_dir(&skills).unwrap();
        for index in 0..70 {
            let name = format!("skill-{index:02}");
            let dir = skills.join(&name);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!(
                    "---\nname: {name}\nversion: 1.0.0\ndescription: 周报\nrequires: []\nevals: []\n---\nhello\n"
                ),
            )
            .unwrap();
        }
        let report = scan_root(&state.store, &skills).await;
        assert_eq!(report.skills.len(), 64, "{report:?}");
        assert_eq!(report.errors.len(), 1, "{report:?}");
        assert_eq!(report.errors[0].path, ".");
        assert!(report.errors[0].message.contains("64"), "{report:?}");
        assert_eq!(state.store.list_skill_revisions().await.unwrap().len(), 128);
    }

    #[tokio::test]
    async fn scans_and_an_escaping_import_eval_name_are_rejected() {
        let state = crate::server::tests::state().await;
        let reserved = "---\nname: scans\ndescription: 周报\n---\nhello\n";
        let (status, value) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(&json!({ "markdown": reserved }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "skill_id_reserved");
        let escaped =
            "---\nname: weekly-report\ndescription: 周报\nevals: ../secret.jsonl\n---\nhello\n";
        let (status, value) = call(
            &state,
            "POST",
            "/api/v1/skills",
            Some(
                &json!({
                    "markdown": escaped,
                    "evals": [{ "name": "../secret.jsonl", "content": "SECRETTOKEN1827\n" }]
                })
                .to_string(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "skill_eval_invalid");
        assert!(!stored_text(&state.store).await.contains("SECRETTOKEN1827"));
        assert!(state.store.list_skill_revisions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rescan_route_rejects_a_path() {
        let state = crate::server::tests::state().await;
        let (status, value) = call(
            &state,
            "POST",
            "/api/v1/skills/scans",
            Some(&json!({ "path": "/etc/passwd" }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "invalid_request");
        assert!(state.store.list_skill_revisions().await.unwrap().is_empty());
    }

    fn walk_names(root: &Path) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                names.insert(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap()
                        .display()
                        .to_string(),
                );
                if entry.file_type().unwrap().is_dir() {
                    pending.push(entry.path());
                }
            }
        }
        names
    }
}
