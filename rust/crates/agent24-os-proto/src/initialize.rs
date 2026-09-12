//! ME-3b-2b — the `initialize` handshake: its wire shape, and how a bad first
//! frame is classified.
//!
//! This module reads ONE frame (given to it as bytes by [`crate::frame`]) and
//! says what it was: a valid handshake request, or a specific kind of refusal.
//! It does not own the connection, so it does not disconnect — SPEC's rule
//! "**any failure during the handshake disconnects**" is carried out by ME-3b-3,
//! which needs this classification in order to log WHY. That split is the reason
//! [`HandshakeError`] is a type and not a string.
//!
//! # The error codes are SPEC's, not this file's
//!
//! From SPEC-ME3 §3 (quoted in the tests, so the mapping is checked rather than
//! asserted):
//!
//! - 首帧不是 `initialize` → `-32600`
//! - 首帧不是合法 JSON → `-32700 Parse error`
//! - 首帧 params 解析失败（含重复 `auth_token` 等重复 JSON key）→ `-32602`
//! - 认证失败 → `-32000` + `kind: auth_failed`
//! - manifest 摘要不符 → `-32000` + `kind: manifest_mismatch`
//! - 版本区间无交集 → `-32000` + `kind: version_mismatch`
//!
//! # The offer set is EMPTY at this slice, and that is deliberate
//!
//! SPEC §8 fixes the ladder: a capability may be offered only once a handler for
//! it exists. `memory` arrives in ME-3d, `events`/`approval` in ME-3e. Offering
//! them here would create the state this design keeps arguing against — granted,
//! but no method behind it. So [`Offer::none`] is what a kernel at this stage
//! answers with, and the type exists so that later slices ADD to it rather than
//! inventing a shape.
//!
//! # This module has no production caller yet
//!
//! 🟢 library-only (SPEC-MD-ME) — **not ✅**. Its consumer is ME-3b-3. The
//! condition for removing it from `main` is an EVENT, not a date: if 3b-3 is
//! abandoned or routed around, this leaves with it.

use serde::{Deserialize, Serialize};

use crate::version::{self, VersionMismatch, VersionRange};

/// The JSON-RPC method name a handshake must use.
pub const INITIALIZE_METHOD: &str = "initialize";

/// What the module sends as the first frame.
///
/// `deny_unknown_fields` is not tidiness: an unrecognised field in a security
/// handshake is a message this kernel does not understand, and accepting it
/// silently is how a future field gets ignored by an old daemon that then
/// reports success. Duplicate keys are rejected by serde's derive for the same
/// reason — SPEC calls out duplicate `auth_token` specifically, because "last
/// one wins" lets a sender show one token to a logger and another to a checker.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InitializeParams {
    /// The module's protocol range. **`None` when the field is absent**, which
    /// SPEC treats as incompatible — not as "assume v1". Modelled as an `Option`
    /// all the way from the wire so the absence survives to `negotiate` instead
    /// of being defaulted at the boundary.
    #[serde(default)]
    pub protocol_versions: Option<VersionRange>,
    /// The module name it claims to be. Checked against the manifest the kernel
    /// spawned; a mismatch is `manifest_mismatch`.
    pub module: String,
    /// Digest of the manifest the module read. The kernel compares it with the
    /// digest of the manifest IT read — two processes disagreeing about the file
    /// that defines identity is not something to proceed past.
    pub manifest_digest: String,
    /// The one-shot secret the kernel passed at spawn.
    pub auth_token: String,
    /// Capabilities the module requests. The kernel answers with what it can
    /// actually serve; see [`Offer`].
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// The full first frame, as JSON-RPC 2.0.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InitializeRequest {
    /// Must be `"2.0"`.
    pub jsonrpc: String,
    /// Must be [`INITIALIZE_METHOD`].
    pub method: String,
    /// Correlation id, echoed in the response. A string — SPEC §3: *"请求 ID
    /// 类型：字符串"* — as on the rest of the callback channel.
    pub id: String,
    /// See [`InitializeParams`].
    pub params: InitializeParams,
}

/// What the kernel offers this connection: the method families that actually
/// have handlers.
///
/// It has to be able to say **"this daemon does not provide `memory.scoped`"**
/// (SPEC §8's acceptance criterion), and it does so by simple absence — a name
/// not in `provides` is not provided. There is no "supported but disabled"
/// state, because that is the same lie in a different spelling.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Offer {
    /// Method-family prefixes the daemon will answer, e.g. `_a24/memory/private`.
    pub provides: Vec<String>,
}

impl Offer {
    /// What a kernel at ME-3b-2b offers: nothing.
    ///
    /// Not a placeholder. At this slice no business handler exists, so any other
    /// answer would grant a capability with no method behind it.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether `method` falls inside something offered.
    #[must_use]
    pub fn provides(&self, method: &str) -> bool {
        self.provides.iter().any(|p| method.starts_with(p.as_str()))
    }
}

/// The kernel's reply to a successful handshake.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InitializeResult {
    /// The single chosen version. SPEC: the kernel must not answer with a
    /// version the module did not declare; [`version::negotiate`] makes that
    /// true by construction.
    pub protocol_version: u32,
    /// See [`Offer`].
    pub offer: Offer,
}

/// Why a first frame was not a usable handshake.
///
/// Each variant carries the JSON-RPC code SPEC assigns it, and the two that are
/// application-level (`-32000`) carry a `kind` from SPEC's closed set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeError {
    /// Not valid JSON at all.
    Parse(String),
    /// Valid JSON, but not an `initialize` request (wrong method, wrong
    /// `jsonrpc`, or not a request object).
    NotInitialize(String),
    /// `params` did not deserialize — a missing field, an unknown field, or a
    /// duplicate key.
    BadParams(String),
    /// The token did not match.
    AuthFailed,
    /// The module's manifest digest or claimed name did not match the kernel's.
    ManifestMismatch { expected: String, got: String },
    /// No shared protocol version, or none declared.
    VersionMismatch(VersionMismatch),
}

impl HandshakeError {
    /// The JSON-RPC error code, per SPEC §3.
    #[must_use]
    pub fn code(&self) -> i32 {
        match self {
            Self::Parse(_) => -32700,
            Self::NotInitialize(_) => -32600,
            Self::BadParams(_) => -32602,
            Self::AuthFailed | Self::ManifestMismatch { .. } | Self::VersionMismatch(_) => -32000,
        }
    }

    /// `error.data.kind` for the application-level failures; `None` for the
    /// protocol-level ones, which are fully described by their code.
    #[must_use]
    pub fn kind(&self) -> Option<&'static str> {
        match self {
            Self::AuthFailed => Some("auth_failed"),
            Self::ManifestMismatch { .. } => Some("manifest_mismatch"),
            Self::VersionMismatch(_) => Some(VersionMismatch::KIND),
            _ => None,
        }
    }
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "the first frame is not valid JSON: {e}"),
            Self::NotInitialize(what) => {
                write!(
                    f,
                    "the first frame must be an `initialize` request, got {what}"
                )
            }
            Self::BadParams(e) => write!(f, "`initialize` params are not usable: {e}"),
            // Deliberately says nothing about the token — not its length, not
            // which half differed. An error message is an oracle if it does.
            Self::AuthFailed => f.write_str("authentication failed"),
            Self::ManifestMismatch { expected, got } => write!(
                f,
                "the module read a different manifest: kernel has {expected}, module reports {got}"
            ),
            Self::VersionMismatch(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HandshakeError {}

/// What the kernel knows before the module says anything.
#[derive(Debug, Clone)]
pub struct Expectation {
    /// The name in the manifest the kernel spawned.
    pub module: String,
    /// The digest of that manifest, as the kernel computed it.
    pub manifest_digest: String,
    /// The one-shot secret handed to the child at spawn.
    pub auth_token: String,
    /// What this kernel build speaks.
    pub kernel_versions: VersionRange,
    /// What it can actually serve — [`Offer::none`] until ME-3d.
    pub offer: Offer,
}

/// Parse and check one first frame.
///
/// The order of checks is deliberate and is asserted in the tests: **shape
/// before secrets**. A frame that is not JSON, or not an `initialize`, is
/// rejected before the token is looked at — otherwise a malformed frame and a
/// wrong token would be distinguishable by timing, and the shape checks would be
/// doing work on attacker-controlled input with a comparison hanging off them.
///
/// # Errors
///
/// See [`HandshakeError`]. Every one of them means the connection must be closed
/// by the caller; this function has no way to do that itself.
pub fn accept(frame: &[u8], expect: &Expectation) -> Result<Accepted, HandshakeError> {
    let (id, params) = envelope(frame)?;
    let params: InitializeParams =
        serde_json::from_value(params).map_err(|e| HandshakeError::BadParams(e.to_string()))?;
    // Identity before secret: a module that read a different manifest is not a
    // module whose token is worth comparing.
    if params.module != expect.module || params.manifest_digest != expect.manifest_digest {
        return Err(HandshakeError::ManifestMismatch {
            expected: format!("{}@{}", expect.module, expect.manifest_digest),
            got: format!("{}@{}", params.module, params.manifest_digest),
        });
    }
    if params.auth_token != expect.auth_token {
        return Err(HandshakeError::AuthFailed);
    }
    let chosen = version::negotiate(params.protocol_versions, expect.kernel_versions)
        .map_err(HandshakeError::VersionMismatch)?;
    Ok(Accepted {
        id,
        result: InitializeResult {
            protocol_version: chosen,
            offer: expect.offer.clone(),
        },
    })
}

/// Check the first frame's JSON-RPC ENVELOPE, and hand back its id and its
/// `params` — before anything reads the params.
///
/// SPEC §3 gives the envelope and the params different codes: a request that
/// is not a valid JSON-RPC `initialize` is `-32600`; an `initialize` whose
/// params do not parse is `-32602`. The first version parsed the whole frame
/// into one struct and tried to tell the two apart from serde's error
/// afterwards, which put `"jsonrpc": 2`, a missing id, an unknown top-level
/// member and a repeated `id` or `method` all under `-32602` (review of
/// ME3-SUP slice 2, round 1). So the envelope is checked here, member by
/// member, on the raw JSON — including repeated keys, which a parsed map
/// cannot show: "last one wins" would answer under whichever id came last.
fn envelope(frame: &[u8]) -> Result<(String, serde_json::Value), HandshakeError> {
    let bad = |what: String| HandshakeError::NotInitialize(what);
    let value: serde_json::Value =
        serde_json::from_slice(frame).map_err(|e| HandshakeError::Parse(e.to_string()))?;
    let serde_json::Value::Object(mut obj) = value else {
        return Err(bad("a frame that is not a request object".to_owned()));
    };
    // Repeated keys, over the raw bytes. A repeat AT the top level is an
    // invalid request, whatever else the frame has. A repeat deeper down is
    // decided only once the envelope has passed below: then every other member
    // is a string or refused, so the repeat can only be inside `params` (bad
    // params). Deciding it here, by path length alone, put `"method": {"x": 1,
    // "x": 2}` under -32602 (review of ME3-SUP slice 2, round 2).
    let repeated = crate::rpc::find_duplicate_key(frame);
    if let Some(path) = repeated.as_ref().filter(|p| p.len() == 1) {
        return Err(bad(format!(
            "a repeated `{}`",
            crate::rpc::clip_to(&path[0], 128)
        )));
    }
    if let Some(extra) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "jsonrpc" | "id" | "method" | "params"))
    {
        return Err(bad(format!(
            "an unknown member `{}`",
            crate::rpc::clip_to(extra, 128)
        )));
    }
    if obj.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err(bad("a `jsonrpc` that is not \"2.0\"".to_owned()));
    }
    match obj.get("method").and_then(serde_json::Value::as_str) {
        Some(INITIALIZE_METHOD) => {}
        Some(other) => {
            return Err(bad(format!("method {:?}", crate::rpc::clip_to(other, 128))));
        }
        None => return Err(bad("a frame with no string `method`".to_owned())),
    }
    let id = match obj.get("id") {
        None => return Err(bad("a frame with no `id`".to_owned())),
        Some(serde_json::Value::String(id)) if id.len() <= crate::rpc::MAX_ID_BYTES => id.clone(),
        Some(serde_json::Value::String(_)) => {
            return Err(bad(format!(
                "an id longer than {} bytes",
                crate::rpc::MAX_ID_BYTES
            )));
        }
        Some(_) => return Err(bad("an id that is not a string".to_owned())),
    };
    let params = obj
        .remove("params")
        .ok_or_else(|| HandshakeError::BadParams("no `params`".to_owned()))?;
    if let Some(path) = repeated {
        return Err(HandshakeError::BadParams(format!(
            "a repeated key `{}`",
            crate::rpc::clip_to(&path.join("."), 128)
        )));
    }
    Ok((id, params))
}

/// A handshake that passed: the request's id, to answer under, and the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub id: String,
    pub result: InitializeResult,
}

/// The id of a first frame, if it has a usable one — for answering a REFUSED
/// handshake under the id the module sent. `None` (answered with `id: null`)
/// for a frame that is not JSON, has no id, has a repeated top-level key (which
/// id would be "the" id is then undecidable), or whose id is not a string
/// within [`crate::rpc::MAX_ID_BYTES`].
#[must_use]
pub fn id_of(frame: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(frame).ok()?;
    if crate::rpc::find_duplicate_key(frame).is_some_and(|p| p.len() == 1) {
        return None;
    }
    let id = v.get("id")?.as_str()?;
    (id.len() <= crate::rpc::MAX_ID_BYTES).then(|| id.to_owned())
}

/// Sent when a line cannot be built — unreachable for today's types, and
/// written out so that a future failure sends a parseable error rather than an
/// empty line.
const FALLBACK_LINE: &[u8] =
    b"{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32603,\"message\":\"internal error\"}}\n";

/// The line answering a handshake that passed.
#[must_use]
pub fn success_line(accepted: &Accepted) -> Vec<u8> {
    match serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": accepted.id,
        "result": accepted.result,
    })) {
        Ok(mut line) => {
            line.push(b'\n');
            line
        }
        Err(_) => FALLBACK_LINE.to_vec(),
    }
}

/// The line answering a refused handshake, sent before the kernel disconnects
/// (SPEC §3: any failure during the handshake disconnects — after saying why).
/// `data.kind` for the application-level refusals; for a version mismatch,
/// both ranges too (SPEC §8: *"把两边区间都放进错误"*).
///
/// Bounded like every other line the kernel writes: the message — which can
/// quote the module (an unknown field's name, the name and digest it claimed)
/// — is cut to [`crate::rpc::MAX_MESSAGE_BYTES`], and a line that would still
/// exceed the frame limit falls back to the code and kind alone (review of
/// ME3-SUP slice 2, round 1: an unbounded message made a refusal the module
/// could not read).
#[must_use]
pub fn error_line(id: Option<&str>, err: &HandshakeError) -> Vec<u8> {
    // An id longer than the channel allows is not echoed, from any caller —
    // `id_of` never yields one, but this function is public and must keep its
    // own bound (review of ME3-SUP slice 2, round 2).
    let id = id.filter(|id| id.len() <= crate::rpc::MAX_ID_BYTES);
    let message = crate::rpc::clip_to(
        &err.to_string(),
        crate::rpc::MAX_MESSAGE_BYTES - '…'.len_utf8(),
    );
    let mut data = err.kind().map(|kind| serde_json::json!({ "kind": kind }));
    if let (Some(data), HandshakeError::VersionMismatch(m)) = (data.as_mut(), err) {
        let range = |r: VersionRange| serde_json::json!({"min": r.min(), "max": r.max()});
        match m {
            VersionMismatch::NotDeclared { kernel } => {
                data["module"] = serde_json::Value::Null;
                data["kernel"] = range(*kernel);
            }
            VersionMismatch::NoOverlap { module, kernel } => {
                data["module"] = range(*module);
                data["kernel"] = range(*kernel);
            }
        }
    }
    let line = |message: &str| {
        let mut error = serde_json::json!({ "code": err.code(), "message": message });
        if let Some(data) = &data {
            error["data"] = data.clone();
        }
        serde_json::to_vec(&serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": error }))
    };
    match line(&message) {
        Ok(mut bytes) if bytes.len() < crate::frame::MAX_FRAME_BYTES => {
            bytes.push(b'\n');
            bytes
        }
        _ => match line("the handshake was refused") {
            Ok(mut bytes) => {
                bytes.push(b'\n');
                bytes
            }
            Err(_) => FALLBACK_LINE.to_vec(),
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::frame::MAX_FRAME_BYTES;

    fn expectation() -> Expectation {
        Expectation {
            module: "cos72".to_owned(),
            manifest_digest: "sha256:abc".to_owned(),
            auth_token: "s3cret".to_owned(),
            kernel_versions: VersionRange::new(1, 1).unwrap(),
            offer: Offer::none(),
        }
    }

    fn good_frame() -> String {
        r#"{"jsonrpc":"2.0","method":"initialize","id":"1","params":{
            "protocol_versions":{"min":1,"max":1},
            "module":"cos72","manifest_digest":"sha256:abc",
            "auth_token":"s3cret","capabilities":["events"]}}"#
            .to_owned()
    }

    /// SPEC §3: *"请求 ID 类型：字符串"*. A numeric id is an invalid request
    /// (-32600), as it is on the rest of the channel; a string is accepted (the
    /// control); an id past the length limit is refused too.
    #[test]
    fn the_handshake_id_is_a_string() {
        let numeric = good_frame().replace(r#""id":"1""#, r#""id":1"#);
        let err = accept(numeric.as_bytes(), &expectation()).expect_err("a numeric id");
        assert_eq!(err.code(), -32600, "{err}");
        let long = good_frame().replace(
            r#""id":"1""#,
            &format!(r#""id":"{}""#, "x".repeat(crate::rpc::MAX_ID_BYTES + 1)),
        );
        let err = accept(long.as_bytes(), &expectation()).expect_err("a long id");
        assert_eq!(err.code(), -32600, "{err}");
        accept(good_frame().as_bytes(), &expectation()).expect("control: a string id");
    }

    #[test]
    fn a_correct_handshake_is_accepted_and_answers_with_one_version() {
        let out = accept(good_frame().as_bytes(), &expectation()).expect("valid handshake");
        assert_eq!(out.result.protocol_version, 1);
        assert_eq!(out.result.offer, Offer::none());
        assert_eq!(out.id, "1", "the id comes back, to answer under");
    }

    /// SPEC-ME3 §3's code assignment, as a table quoted from the document.
    ///
    /// The expected codes are not the author's to choose — the quotations sit
    /// beside the rows they govern, so a reader can check the claim without
    /// leaving this file. (The pattern is the one set by `version::tests`.)
    #[test]
    fn the_spec_error_codes() {
        let e = expectation();
        let case = |frame: &str| accept(frame.as_bytes(), &e).expect_err("must refuse");

        // SPEC: 「首帧不是合法 JSON → `-32700 Parse error` 并断连」
        let parse = case("{not json");
        assert_eq!(parse.code(), -32700, "{parse}");
        assert_eq!(parse.kind(), None);

        // SPEC: 「**首帧不是 `initialize`** → `-32600` 断连」
        //
        // TWO frames, because there are two paths to this code and only one of
        // them was covered. This frame's params are incomplete, so it fails
        // strict deserialisation and is classified by `classify()` — it never
        // reaches the `method` check inside `accept`. Measured (review): with
        // only this row, deleting that check left the whole suite green.
        let wrong_method =
            case(r#"{"jsonrpc":"2.0","method":"ping","id":"1","params":{"module":"cos72"}}"#);
        assert_eq!(wrong_method.code(), -32600, "{wrong_method}");
        // …and the frame that DOES reach it: valid everything, wrong method.
        let wrong_method_complete = case(&good_frame().replace("\"initialize\"", "\"ping\""));
        assert_eq!(
            wrong_method_complete.code(),
            -32600,
            "a fully-formed frame with the wrong method was accepted: {wrong_method_complete}"
        );

        // SPEC: 「首帧 params 解析失败（含重复 `auth_token` 等重复 JSON key）→
        //        `-32602` 并断连」
        let missing_field =
            case(r#"{"jsonrpc":"2.0","method":"initialize","id":"1","params":{"module":"cos72"}}"#);
        assert_eq!(missing_field.code(), -32602, "{missing_field}");

        // SPEC: 「认证失败 → `-32000` + `kind: auth_failed` 并断连」
        let bad_token = case(&good_frame().replace("s3cret", "wrong"));
        assert_eq!(bad_token.code(), -32000);
        assert_eq!(bad_token.kind(), Some("auth_failed"));

        // SPEC: 「manifest 摘要不符 → `-32000` + `kind: manifest_mismatch` 并断连」
        let bad_digest = case(&good_frame().replace("sha256:abc", "sha256:zzz"));
        assert_eq!(bad_digest.code(), -32000);
        assert_eq!(bad_digest.kind(), Some("manifest_mismatch"));

        // SPEC: 「交集为空 → 握手失败並把两边区间都放进错误（`-32000` +
        //        `version_mismatch`）」／「模块**不报区间** → 视为不兼容」
        let no_overlap = case(&good_frame().replace(r#""min":1,"max":1"#, r#""min":7,"max":9"#));
        assert_eq!(no_overlap.code(), -32000);
        assert_eq!(no_overlap.kind(), Some("version_mismatch"));
        let undeclared =
            case(&good_frame().replace(r#""protocol_versions":{"min":1,"max":1},"#, ""));
        assert_eq!(undeclared.kind(), Some("version_mismatch"), "{undeclared}");

        // Control: every code above must be reachable and they must not all be
        // the same number — a `code()` returning one constant would pass any
        // single row.
        let codes = [
            parse.code(),
            wrong_method.code(),
            missing_field.code(),
            bad_token.code(),
        ];
        assert_eq!(codes.len(), 4);
        assert!(
            codes
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == 4
        );
    }

    /// SPEC calls out duplicate `auth_token` by name. "Last one wins" lets a
    /// sender show one token to whatever logs the frame and a different one to
    /// whatever checks it.
    ///
    /// The rejection comes from serde's derive, not from code written here — so
    /// this test's real job is to prove that inherited behaviour is actually in
    /// force, with a control showing the same frame is otherwise fine.
    #[test]
    fn a_duplicate_key_is_refused_rather_than_last_one_wins() {
        let dup = good_frame().replace(
            r#""auth_token":"s3cret""#,
            r#""auth_token":"decoy","auth_token":"s3cret""#,
        );
        let err =
            accept(dup.as_bytes(), &expectation()).expect_err("duplicate key must be refused");
        assert_eq!(err.code(), -32602, "{err}");
        // Control: without the duplication the same frame is accepted, so the
        // refusal above is about the duplicate and not about the edit.
        assert!(accept(good_frame().as_bytes(), &expectation()).is_ok());
    }

    /// An unknown field is a message this kernel does not understand.
    #[test]
    fn an_unknown_field_is_refused_not_ignored() {
        let extra = good_frame().replace(r#""capabilities""#, r#""future_field":1,"capabilities""#);
        let err = accept(extra.as_bytes(), &expectation()).expect_err("unknown field");
        assert_eq!(err.code(), -32602, "{err}");
    }

    /// Shape is checked before secrets: a frame that is not a valid `initialize`
    /// must be refused as such even when its token is also wrong, so that the
    /// two failures cannot be told apart by which check ran.
    #[test]
    fn the_shape_is_checked_before_the_token() {
        let both_wrong = r#"{"jsonrpc":"2.0","method":"ping","id":"1","params":{"module":"x"}}"#;
        let err = accept(both_wrong.as_bytes(), &expectation()).unwrap_err();
        assert_eq!(err.code(), -32600, "the token was consulted first: {err}");
    }

    /// The auth failure must not describe the token.
    #[test]
    fn the_auth_failure_is_not_an_oracle() {
        let err = accept(
            good_frame().replace("s3cret", "wrong").as_bytes(),
            &expectation(),
        )
        .unwrap_err();
        let text = err.to_string();
        for leak in ["s3cret", "wrong", "6", "length"] {
            assert!(!text.contains(leak), "{text} leaks {leak:?}");
        }
    }

    /// The offer set must be able to say "this daemon does not provide
    /// `memory.scoped`" — SPEC §8's acceptance criterion for capability
    /// negotiation. Absence IS the statement; there is no "supported but off".
    #[test]
    fn an_unoffered_method_family_is_simply_absent() {
        let empty = Offer::none();
        assert!(!empty.provides("_a24/memory/scoped/get"));
        assert!(!empty.provides("_a24/memory/private/get"));
        // Control: the predicate is not constantly false.
        let later = Offer {
            provides: vec!["_a24/memory/private".to_owned()],
        };
        assert!(later.provides("_a24/memory/private/get"));
        assert!(
            !later.provides("_a24/memory/scoped/get"),
            "offering private must not imply scoped"
        );
    }

    /// **FU-42.** `MAX_FRAME_BYTES` was chosen before this message existed, and
    /// exceeding it disconnects rather than degrades. So the number is pinned
    /// here, against the largest `initialize` this protocol can actually
    /// produce, rather than against the small one used in the other tests.
    ///
    /// The control matters as much as the measurement: a frame deliberately over
    /// the limit must measure over it, or "it fits" would be a statement about
    /// the ruler.
    #[test]
    fn the_largest_possible_initialize_fits_in_one_frame() {
        // Every capability this design will ever offer (SPEC §8's final offer
        // set), a generously long module name and digest, and a token far larger
        // than anything the kernel generates.
        let params = InitializeParams {
            protocol_versions: VersionRange::new(1, u32::MAX),
            // The real bound, not a guessed one: `valid_name` enforces
            // `MAX_NAME_BYTES`. The first version used
            // `MAX_YAML_BYTES.min(4096)` — 64× too large, and mixing two
            // different quantities (a whole document's cap vs one name's).
            // Over-wide is safe in DIRECTION but bites later: the slack it eats
            // is slack that does not exist, so a future field could turn this
            // red over a module name that can never occur, and the next person
            // would edit the test.
            module: "m".repeat(agent24_domain::MAX_NAME_BYTES),
            manifest_digest: format!("sha512:{}", "f".repeat(128)),
            // ASSUMPTION, not a measurement: the kernel's token generator does
            // not exist yet (see FU-44). Replace this with the real bound when
            // 3b-3 lands. A number labelled as an assumption and a number that
            // looks like a reading cost very different amounts to be wrong about.
            auth_token: "t".repeat(4096),
            capabilities: vec![
                "memory.private".to_owned(),
                "memory.scoped".to_owned(),
                "events".to_owned(),
                "approval".to_owned(),
            ],
        };
        let req = InitializeRequest {
            jsonrpc: "2.0".to_owned(),
            method: INITIALIZE_METHOD.to_owned(),
            // The longest id accepted (ids are strings, SPEC §3).
            id: "x".repeat(crate::rpc::MAX_ID_BYTES),
            params,
        };
        let encoded = serde_json::to_vec(&req).unwrap();
        assert!(
            encoded.len() < MAX_FRAME_BYTES,
            "the largest initialize is {} bytes, at or over the {MAX_FRAME_BYTES}-byte frame limit \
             — raise the limit BEFORE this lands, because exceeding it disconnects",
            encoded.len()
        );
        // Positive control: the same ruler, applied to something known to be over
        // the limit, must say so.
        let oversized = vec![b'x'; MAX_FRAME_BYTES + 1];
        assert!(oversized.len() > MAX_FRAME_BYTES);
    }

    /// Every envelope fault is an invalid request (-32600), not bad params —
    /// the first version sent all of these to -32602 (review of ME3-SUP slice
    /// 2, round 1). A repeated top-level key counts as one: "last one wins"
    /// would answer under whichever id came last. The good frame is the
    /// control, and a repeat INSIDE params stays -32602.
    #[test]
    fn envelope_faults_are_invalid_requests_not_bad_params() {
        let g = good_frame();
        let cases = [
            (
                "jsonrpc a number",
                g.replace(r#""jsonrpc":"2.0""#, r#""jsonrpc":2"#),
            ),
            ("no id", g.replace(r#""id":"1","#, "")),
            (
                "an unknown member",
                g.replace(r#""id":"1","#, r#""id":"1","extra":true,"#),
            ),
            (
                "a repeated id",
                g.replace(r#""id":"1","#, r#""id":"1","id":"2","#),
            ),
            (
                "a repeated method",
                g.replace(
                    r#""method":"initialize","#,
                    r#""method":"ping","method":"initialize","#,
                ),
            ),
        ];
        for (what, frame) in cases {
            let err = accept(frame.as_bytes(), &expectation()).expect_err(what);
            assert_eq!(err.code(), -32600, "{what}: {err}");
        }
        let repeated_token = g.replace(
            r#""auth_token":"s3cret""#,
            r#""auth_token":"x","auth_token":"s3cret""#,
        );
        assert_eq!(
            accept(repeated_token.as_bytes(), &expectation())
                .unwrap_err()
                .code(),
            -32602,
            "a repeat inside params is bad params"
        );
        accept(g.as_bytes(), &expectation()).expect("control");
    }

    /// With a repeated top-level key there is no one id to answer under: the
    /// refusal goes out with `id: null`. A single id comes back (the control).
    #[test]
    fn a_refusal_is_answered_under_null_when_the_id_is_ambiguous() {
        let g = good_frame();
        assert_eq!(id_of(g.as_bytes()).as_deref(), Some("1"));
        let repeated = g.replace(r#""id":"1","#, r#""id":"1","id":"2","#);
        assert_eq!(id_of(repeated.as_bytes()), None);
    }

    /// A refusal line stays within the frame limit, however much of the module
    /// it quotes: a manifest name near the frame size is cut, not echoed.
    #[test]
    fn a_refusal_that_quotes_the_module_stays_small() {
        let long = "m".repeat(MAX_FRAME_BYTES - 1000);
        let frame = good_frame().replace(r#""module":"cos72""#, &format!(r#""module":"{long}""#));
        assert!(
            frame.len() < MAX_FRAME_BYTES,
            "precondition: the frame itself is legal"
        );
        let err = accept(frame.as_bytes(), &expectation()).expect_err("a different name");
        assert!(
            matches!(err, HandshakeError::ManifestMismatch { .. }),
            "{err}"
        );
        let line = error_line(id_of(frame.as_bytes()).as_deref(), &err);
        assert!(line.len() < 2048, "{} bytes", line.len());
        let v: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(v["error"]["data"]["kind"], "manifest_mismatch");
    }

    /// A repeat nested inside a member that is not `params` is an envelope
    /// fault (-32600), because that member is itself wrong: a `method`, `id`
    /// or unknown member that is an object. The first envelope check decided
    /// by path length alone and said -32602 (review of ME3-SUP slice 2,
    /// round 2).
    #[test]
    fn a_repeat_inside_a_non_params_member_is_an_envelope_fault() {
        let g = good_frame();
        let cases = [
            (
                "method",
                g.replace(r#""method":"initialize""#, r#""method":{"x":1,"x":2}"#),
            ),
            ("id", g.replace(r#""id":"1""#, r#""id":{"x":1,"x":2}"#)),
            (
                "unknown member",
                g.replace(r#""id":"1","#, r#""id":"1","extra":{"x":1,"x":2},"#),
            ),
        ];
        for (what, frame) in cases {
            let err = accept(frame.as_bytes(), &expectation()).expect_err(what);
            assert_eq!(err.code(), -32600, "{what}: {err}");
        }
    }

    /// `error_line` keeps its bound for any caller: an id past the limit is
    /// answered as `null`, not echoed.
    #[test]
    fn an_overlong_id_is_not_echoed_by_error_line() {
        let huge = "i".repeat(MAX_FRAME_BYTES);
        let line = error_line(Some(&huge), &HandshakeError::AuthFailed);
        assert!(line.len() < 1024, "{} bytes", line.len());
        let v: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(v["id"], serde_json::Value::Null);
        // Control: an id within the limit is echoed.
        let v: serde_json::Value =
            serde_json::from_slice(&error_line(Some("ok"), &HandshakeError::AuthFailed)).unwrap();
        assert_eq!(v["id"], "ok");
    }
}
