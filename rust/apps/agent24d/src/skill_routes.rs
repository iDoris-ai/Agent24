//! Skill import and user-directory scan (skill-loader SL-3).
//!
//! `POST /api/v1/skills` accepts JSON (`markdown`, optional `evals`). It does
//! not accept a path and does not open one. Import is always `origin=imported`
//! and goes through [`agent24_skill::parse_imported`].
//!
//! The scanner reads `<data_dir>/skills/<name>/` only. It rejects `..`,
//! absolute paths, symlinks, and non-regular files before inserting, and it
//! caps a skill directory at 32 regular files and 256 KiB. Validation, the
//! content digest, and the stored eval bytes all come from that one snapshot.
//!
//! Neither path moves the skill pointer. Activation is SL-4. Provider status
//! is SL-5 and is omitted here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
    /// Text-consistency counts for the newest body paired with the evals row
    /// of the same origin and semver. Provider status is SL-5 and is omitted.
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
    let Some(dir) = agent24_protocol::state_file::state_dir() else {
        return ScanReport::empty();
    };
    scan_root(store, &dir.join("skills")).await
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
    if evals.len() != 1 || evals[0].name != manifest.evals {
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
    let body = NewSkillRevision {
        revision_id: uuid::Uuid::new_v4().to_string(),
        skill_id: manifest.name.clone(),
        origin,
        semver: manifest.version.clone(),
        kind: SkillRevisionKind::Body,
        content_digest: manifest.content_digest(),
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
            content_digest: manifest.content_digest(),
            ineffective,
            inserted,
            pointer: pointer.map(pointer_view),
            generation,
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
    let text_consistency = text_consistency(store, &mine).await?;
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

async fn text_consistency(
    store: &Store,
    rows: &[agent24_store::SkillRevisionMeta],
) -> Result<Option<ConsistencyView>, Fail> {
    let body = rows
        .iter()
        .rev()
        .find(|row| row.kind == SkillRevisionKind::Body);
    let Some(body) = body else {
        return Ok(None);
    };
    let evals = rows.iter().rev().find(|row| {
        row.kind == SkillRevisionKind::Evals
            && row.origin == body.origin
            && row.semver == body.semver
    });
    let Some(evals) = evals else {
        return Ok(None);
    };
    let body_row = store
        .get_skill_revision(&body.revision_id)
        .await
        .map_err(|_| Fail::internal("read body"))?;
    let eval_row = store
        .get_skill_revision(&evals.revision_id)
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
    let Ok(manifest) = parse_for_origin(body.origin, &markdown) else {
        return Ok(None);
    };
    if manifest.evals.is_empty() || eval_text.is_empty() {
        return Ok(Some(ConsistencyView {
            consistent: 0,
            total: 0,
        }));
    }
    match agent24_skill::check_text_consistency(
        &manifest.evals,
        &eval_text,
        &manifest.body,
        skill_origin(body.origin),
        &manifest.content_digest(),
    ) {
        Ok(report) => {
            let (consistent, total) = report.consistent_counts();
            Ok(Some(ConsistencyView { consistent, total }))
        }
        Err(SkillError::EvalEmpty) => Ok(Some(ConsistencyView {
            consistent: 0,
            total: 0,
        })),
        Err(_) => Ok(None),
    }
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
            other => Self::Http(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                other.to_string(),
            ),
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

fn prepare_root(root: &Path) -> Vec<Prepared> {
    let meta = match std::fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            return vec![failed(
                root.display().to_string(),
                "skill_manifest_invalid",
                &err.to_string(),
            )];
        }
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return vec![failed(
            root.display().to_string(),
            "skill_manifest_invalid",
            "skills root is not a directory",
        )];
    }
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) => {
            return vec![failed(
                root.display().to_string(),
                "skill_manifest_invalid",
                &err.to_string(),
            )];
        }
    };
    let mut prepared = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                prepared.push(failed(
                    root.display().to_string(),
                    "skill_manifest_invalid",
                    &err.to_string(),
                ));
                continue;
            }
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            prepared.push(failed(
                root.display().to_string(),
                "skill_manifest_invalid",
                "skill directory name is not utf-8",
            ));
            continue;
        };
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
        {
            prepared.push(failed(
                name,
                "skill_manifest_invalid",
                "skill directory name escapes",
            ));
            continue;
        }
        let dir = root.join(&name);
        let meta = match std::fs::symlink_metadata(&dir) {
            Ok(meta) => meta,
            Err(err) => {
                prepared.push(failed(&name, "skill_manifest_invalid", &err.to_string()));
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            prepared.push(failed(&name, "skill_manifest_invalid", "symlink rejected"));
            continue;
        }
        if !meta.is_dir() {
            continue;
        }
        prepared.push(prepare_skill(&name, &dir));
    }
    prepared
}

fn prepare_skill(name: &str, dir: &Path) -> Prepared {
    let walked = match walk_skill(dir) {
        Ok(walked) => walked,
        Err(message) => return failed(name, "skill_manifest_invalid", &message),
    };
    let mut ingested = BTreeMap::new();
    let mut skipped = Vec::new();
    for (rel, _) in &walked {
        if is_ingested(rel) {
            let bytes = match read_nofollow(&abs_from(dir, rel)) {
                Ok(bytes) => bytes,
                Err(err) => return failed(name, "skill_manifest_invalid", &err.to_string()),
            };
            let Ok(text) = String::from_utf8(bytes) else {
                return failed(name, "skill_manifest_invalid", "skill file is not utf-8");
            };
            ingested.insert(rel.clone(), text);
        } else {
            skipped.push(rel.clone());
        }
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

fn walk_skill(dir: &Path) -> Result<Vec<(String, u64)>, String> {
    let mut files = Vec::new();
    let mut pending = vec![String::new()];
    while let Some(rel_dir) = pending.pop() {
        let abs = if rel_dir.is_empty() {
            dir.to_path_buf()
        } else {
            abs_from(dir, &rel_dir)
        };
        let entries = std::fs::read_dir(&abs)
            .map_err(|err| format!("skill directory cannot be read: {err}"))?;
        for entry in entries {
            let entry = entry.map_err(|err| format!("skill directory cannot be read: {err}"))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                return Err("skill directory entry is not utf-8".to_owned());
            };
            if name.is_empty()
                || name == "."
                || name == ".."
                || name.contains('/')
                || name.contains('\\')
            {
                return Err(format!("skill directory entry escapes: {name}"));
            }
            let rel = if rel_dir.is_empty() {
                name.clone()
            } else {
                format!("{rel_dir}/{name}")
            };
            let meta =
                std::fs::symlink_metadata(abs_from(dir, &rel)).map_err(|err| err.to_string())?;
            let kind = meta.file_type();
            if kind.is_symlink() {
                return Err(format!("symlink rejected: {rel}"));
            }
            if kind.is_dir() {
                pending.push(rel);
                continue;
            }
            if !kind.is_file() {
                return Err(format!("not a regular file: {rel}"));
            }
            files.push((rel, meta.len()));
            if files.len() > MAX_FILES {
                return Err("skill directory exceeds 32 files".to_owned());
            }
            let total: u64 = files.iter().map(|(_, len)| *len).sum();
            if total > MAX_TOTAL_BYTES {
                return Err("skill directory exceeds 256 KiB".to_owned());
            }
        }
    }
    Ok(files)
}

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

fn abs_from(root: &Path, rel: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in rel.split('/') {
        path.push(part);
    }
    path
}

fn read_nofollow(path: &Path) -> std::io::Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        let flag = if cfg!(target_os = "linux") {
            0x20000
        } else {
            0x0100
        };
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(flag)
            .open(path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Ok(buf)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "skill scan requires unix",
        ))
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
        assert_eq!(detail["text_consistency"]["consistent"], 0);
        assert_eq!(detail["text_consistency"]["total"], 0);
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
        assert_eq!(detail["text_consistency"]["consistent"], 1);
        assert_eq!(detail["text_consistency"]["total"], 1);
        assert!(!detail.to_string().contains("NOTEONLY"));
    }

    #[tokio::test]
    async fn rescan_route_rejects_a_path() {
        let state = crate::server::tests::state().await;
        let (status, value) = call(&state, "POST", "/api/v1/skills/scans", Some("{}")).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["origin"], "user");
        assert!(value["skills"].is_array());
        assert!(value["errors"].is_array());
        let (status, value) = call(
            &state,
            "POST",
            "/api/v1/skills/scans",
            Some(&json!({ "path": "/etc/passwd" }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "invalid_request");
    }

    #[test]
    fn serve_scans_the_user_directory_on_startup() {
        let src = include_str!("server.rs");
        let start = src.find("pub async fn serve(").unwrap();
        let body = &src[start..];
        let end = body.find("\n}\n").unwrap();
        assert!(body[..end].contains("scan_installed_user_skills"));
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
