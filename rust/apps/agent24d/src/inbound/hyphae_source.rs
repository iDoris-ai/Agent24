//! Hyphae inbound adapter (X11-2e).
//!
//! Polls the local Hyphae CLI and calls [`super::service::InboundService::ingest`].
//! The CLI is a [`HyphaeCli`] so tests never start a real `hyphae`. `agent24-comm`
//! stays transport-only: this module does not add a dependency and does not
//! change that crate's allowlist.
//!
//! # Fields checked against Hyphae v0.30.1 (local tree `v0.30.1-25-g2a57be27`)
//!
//! DM `history inbox --json` (no password). `data` is a JSON array, oldest
//! not guaranteed: `GetInbox` orders `created_at DESC` and there is no
//! `--after-id` (`internal/storage/message.go`, JOINT-ROUND1, COMM-FRIENDS).
//! Each element is `StoredMessage` (`pkg/types/types.go`):
//! `id` (this is the hex event id; JOINT-ROUND1: it matches `agent msg`'s
//! `event_id`), `sender_npub`, `recipient_npub`, `content`, `plaintext`,
//! `created_at` (unix seconds), `received_at` (unix seconds, not used for
//! `at`), `is_encrypted`, `is_incoming`, `relay`. No `kind`, no mention
//! list. Ordinary DMs are also kind 30078, so kind is not a protocol bit
//! (COMM-HYPHAE).
//!
//! Group `groupchat history --json` needs the keystore password. `data` is
//! `{"messages":[...]}`, oldest first. `GroupMessage` (`pkg/types/group.go`,
//! same fields as Hyphae `docs/agent/group-chat-agent24-interface-20261025.md`):
//! `id` (logical id, the `--after-id` / `--before-id` cursor), `event_id`
//! (empty on the sender's own row), `group_id`, `sender` (npub), `content`,
//! `plaintext`, `created_at` (unix seconds), `is_encrypted`. No mention
//! field. `--after-id` returns messages newer than that logical id, oldest
//! of those first. Unknown cursor is a user error.
//!
//! `identity list` `data` is an array of `{nickname, npub, ...}`.
//! `groupchat list` `data` is `{"groups":[{"group_id", ...}]}`.
//!
//! Unverified and therefore conservative: a group @ is only the local npub
//! or `@nickname` in the text (no mention array → not a mention →
//! `addressed=none`). `@nickname` needs a left boundary (start, whitespace,
//! or punctuation), so `bob@me.com` is not a mention. There is no
//! per-message human/bot flag; `human` is the content rule below, and a
//! body we cannot classify as protocol is `human=true` (the written §3.3
//! rule). An empty `event_id` is not ingested. A body whose trimmed text
//! starts with `hyphae.group/` is protocol: Hyphae already drops that
//! prefix on its three receive paths; this is the same check on our side.
//!
//! # When the poll runs
//!
//! Jason 2026-11-12: the default is not to pull. A round pulls only when
//! the comm slot currently has a CLI and either `inbound_allowlist` has a
//! `source=hyphae` row, or `inbound_policy` exists with `accept_mode` of
//! `observe` or `auto` (a value the user stored). No policy row and an
//! empty allowlist does not pull. A stored `accept_mode=off` always stops
//! pulling, allowlist or not: off means off. The decision and the CLI are
//! read every round, not frozen at process start.
//!
//! # `human`
//!
//! `human=true` only when the body is not a machine protocol message.
//! Protocol (always `category=protocol`, `human=false`, admit
//! `ignore(protocol)`): JSON `version=="f4/1"` (ask envelope and ack
//! receipt), JSON `protocol=="hyphae-behavior/1"` with `type` `query` or
//! `response`, a body whose trimmed text starts with `a24-liveness-canary`,
//! or a body whose trimmed text starts with `agent24.friend/v1`. Kind is
//! ignored. Anything else is human. The model is not asked.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use agent24_agent::RunManager;
use agent24_store::{Store, format_inbound_timestamp};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::service::{INBOUND_MAX_AGE_SECS, InboundMessage, InboundService};

/// Background poll period. Fixed; the task also wakes on the stop token.
pub(crate) const HYPHAE_POLL_INTERVAL: Duration = Duration::from_secs(15);
/// `history inbox` / `groupchat history` page size. Comm caps both at 200.
const HISTORY_LIMIT: u32 = 200;
/// How many group pages one poll will walk so a backlog can catch up.
const MAX_GROUP_PAGES: u32 = 8;
/// Locked keystore / missing binary: warn at most this often, per category.
const QUIET_WARN_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Same unreadable group row for this many rounds, then skip it.
const STUCK_ROUNDS: u32 = 3;

const SOURCE: &str = "hyphae";
const CANARY_PREFIX: &str = "a24-liveness-canary";
const FRIEND_PREFIX: &str = "agent24.friend/v1";
const GROUP_PREFIX: &str = "hyphae.group/";
/// Hyphae `ErrCursorNotFound` (`internal/groupchat/store.go`).
const UNKNOWN_CURSOR: &str = "history cursor is not a message of this group";

/// Something the poller calls at the start of every round to get the CLI
/// the comm slot currently holds. `None` means Hyphae is not configured.
pub(crate) type HyphaeCliSource = Arc<dyn Fn() -> Option<Arc<dyn HyphaeCli>> + Send + Sync>;

/// One Hyphae invocation. `with_password` asks the caller to pass the
/// keystore password on stdin (`groupchat` needs it; `history inbox` and
/// `identity list` do not).
#[async_trait::async_trait]
pub(crate) trait HyphaeCli: Send + Sync {
    async fn run(&self, args: &[String], with_password: bool) -> Result<Value, HyphaeCliError>;
}

/// Why a CLI call did not return data. `Failed` is a short message, not the
/// child output.
#[derive(Debug, Clone)]
pub(crate) enum HyphaeCliError {
    /// No verified binary, or the process could not be spawned.
    NotInstalled,
    /// Keystore password missing, or Hyphae exit `auth_error`.
    Locked,
    Failed(String),
}

impl HyphaeCliError {
    fn text(&self) -> String {
        match self {
            Self::NotInstalled => "hyphae is not installed".to_owned(),
            Self::Locked => "hyphae keystore is locked".to_owned(),
            Self::Failed(message) => message.clone(),
        }
    }
}

/// Start the poll loop. It exits when `cancel` fires. The join handle is
/// detached. Whether a round pulls is decided inside the loop.
pub(crate) async fn start_hyphae_inbound(
    store: &Store,
    runs: Arc<RunManager>,
    cli: impl Fn() -> Option<Arc<dyn HyphaeCli>> + Send + Sync + 'static,
    cancel: CancellationToken,
) {
    let source = HyphaeInbound::new(store.clone(), runs, Arc::new(cli), Arc::new(Utc::now));
    tokio::spawn(source.run(HYPHAE_POLL_INTERVAL, cancel));
}

/// Pull this round. `accept_mode` is the stored row, or `None` when the
/// user has not written one. `hyphae_allowlisted` is "at least one
/// `source=hyphae` allowlist row". Either opt-in is enough; a stored `off`
/// overrides both.
pub(crate) fn hyphae_poll_enabled(accept_mode: Option<&str>, hyphae_allowlisted: bool) -> bool {
    match accept_mode {
        Some("off") => false,
        Some("observe" | "auto") => true,
        _ => hyphae_allowlisted,
    }
}

struct HyphaeInbound {
    store: Store,
    service: InboundService,
    cli: HyphaeCliSource,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    /// Last warn instant per category. Categories do not share a timer.
    warned_at: Mutex<HashMap<String, std::time::Instant>>,
    /// Consecutive rounds stuck on the same unreadable group row.
    stuck: Mutex<HashMap<(String, String), (String, u32)>>,
    /// DMs that reached ingest. Tests assert a repeat page does not bump it.
    direct_attempts: std::sync::atomic::AtomicUsize,
}

impl HyphaeInbound {
    fn new(
        store: Store,
        runs: Arc<RunManager>,
        cli: HyphaeCliSource,
        now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    ) -> Self {
        let service = InboundService::new(store.clone(), runs);
        Self {
            store,
            service,
            cli,
            now,
            warned_at: Mutex::new(HashMap::new()),
            stuck: Mutex::new(HashMap::new()),
            direct_attempts: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    async fn run(self, interval: Duration, cancel: CancellationToken) {
        loop {
            if cancel.is_cancelled() {
                break;
            }
            if let Err(err) = self.poll_round().await {
                self.quiet_warn("store", &err);
            }
            tokio::select! {
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(interval) => {}
            }
        }
    }

    /// One round. Opt-in and the current CLI are read here, then the pull.
    async fn poll_round(&self) -> Result<(), String> {
        let mode = self
            .store
            .inbound_accept_mode()
            .await
            .map_err(|err| err.to_string())?;
        let allowlisted = self
            .store
            .inbound_source_allowlisted(SOURCE)
            .await
            .map_err(|err| err.to_string())?;
        if !hyphae_poll_enabled(mode.as_deref(), allowlisted) {
            return Ok(());
        }
        let Some(cli) = (self.cli)() else {
            return Ok(());
        };
        self.poll_once(cli.as_ref()).await
    }

    async fn poll_once(&self, cli: &dyn HyphaeCli) -> Result<(), String> {
        let now = (self.now)();
        let stamp = format_inbound_timestamp(now);
        self.store
            .ensure_inbound_watermark(
                SOURCE,
                &stamp,
                i64::try_from(INBOUND_MAX_AGE_SECS).unwrap_or(i64::MAX),
            )
            .await
            .map_err(|err| err.to_string())?;
        let identities = match cli.run(&args(&["identity", "list"]), false).await {
            Ok(data) => match identity_rows(&data) {
                Some(rows) => rows,
                None => {
                    self.quiet_warn("identity", "identity list was not an array");
                    return Ok(());
                }
            },
            Err(err) => {
                self.note_cli(err);
                return Ok(());
            }
        };
        let locals: HashSet<String> = identities.iter().map(|(_, npub)| npub.clone()).collect();
        for (nickname, npub) in &identities {
            if let Err(err) = self.poll_direct(cli, nickname, npub, &locals).await {
                self.note_cli(err);
            }
            if let Err(err) = self.poll_groups(cli, nickname, npub, &locals).await {
                self.note_cli(err);
            }
        }
        Ok(())
    }

    async fn poll_direct(
        &self,
        cli: &dyn HyphaeCli,
        nickname: &str,
        npub: &str,
        locals: &HashSet<String>,
    ) -> Result<(), HyphaeCliError> {
        let data = cli
            .run(&args_limit(&["history", "inbox", "--as", nickname]), false)
            .await?;
        let Some(mut rows) = array_rows(&data) else {
            return Err(HyphaeCliError::Failed(
                "history inbox data is not an array".to_owned(),
            ));
        };
        // Inbox is newest first. The earliest of a burst has to be the one
        // that can start a run.
        rows.sort_by(|left, right| direct_order(left).cmp(&direct_order(right)));
        for row in rows {
            if own_direct(row, locals) {
                continue;
            }
            if let Some(event_id) = row
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            {
                let stored_id = format!("hyphae:{event_id}");
                match self.store.get_inbound_event(&stored_id).await {
                    Ok(Some(_)) => continue,
                    Ok(None) => {}
                    Err(err) => {
                        self.quiet_warn("store", &format!("direct lookup {stored_id}: {err}"));
                        continue;
                    }
                }
            }
            self.count_direct_attempt();
            self.ingest_direct(npub, nickname, row).await;
        }
        Ok(())
    }

    async fn poll_groups(
        &self,
        cli: &dyn HyphaeCli,
        nickname: &str,
        npub: &str,
        locals: &HashSet<String>,
    ) -> Result<(), HyphaeCliError> {
        let data = cli
            .run(&args(&["groupchat", "list", "--identity", nickname]), true)
            .await?;
        let Some(groups) = active_group_ids(&data) else {
            return Err(HyphaeCliError::Failed(
                "groupchat list data has no groups array".to_owned(),
            ));
        };
        for group_id in groups {
            if let Err(err) = self
                .poll_group(cli, nickname, npub, &group_id, locals)
                .await
            {
                self.note_group_error(cli, nickname, npub, &group_id, locals, err)
                    .await;
            }
        }
        Ok(())
    }

    /// One group's history failed. An unknown cursor is dropped and that
    /// group is retried once. Anything else is a rate-limited warning.
    /// The next group still runs. The watermark is not touched.
    async fn note_group_error(
        &self,
        cli: &dyn HyphaeCli,
        nickname: &str,
        npub: &str,
        group_id: &str,
        locals: &HashSet<String>,
        err: HyphaeCliError,
    ) {
        if unknown_cursor(&err) {
            match self
                .store
                .clear_inbound_cursor(SOURCE, npub, group_id)
                .await
            {
                Ok(()) => {
                    if let Err(retry) = self.poll_group(cli, nickname, npub, group_id, locals).await
                    {
                        self.quiet_warn(
                            &format!("group:{group_id}"),
                            &format!(
                                "group {group_id} history after clearing cursor: {}",
                                retry.text()
                            ),
                        );
                    }
                }
                Err(clear_err) => {
                    self.quiet_warn(
                        "store",
                        &format!("group {group_id} clear cursor: {clear_err}"),
                    );
                }
            }
            return;
        }
        self.quiet_warn(
            &format!("group:{group_id}"),
            &format!("group {group_id} history: {}", err.text()),
        );
    }

    async fn poll_group(
        &self,
        cli: &dyn HyphaeCli,
        nickname: &str,
        npub: &str,
        group_id: &str,
        locals: &HashSet<String>,
    ) -> Result<(), HyphaeCliError> {
        for _ in 0..MAX_GROUP_PAGES {
            let cursor = self
                .store
                .inbound_cursor(SOURCE, npub, group_id)
                .await
                .map_err(|err| HyphaeCliError::Failed(format!("store: {err}")))?;
            let mut argv = vec![
                "groupchat".to_owned(),
                "history".to_owned(),
                "--group-id".to_owned(),
                group_id.to_owned(),
                "--identity".to_owned(),
                nickname.to_owned(),
                "--limit".to_owned(),
                HISTORY_LIMIT.to_string(),
            ];
            if let Some(cursor) = cursor.as_deref() {
                argv.push("--after-id".to_owned());
                argv.push(cursor.to_owned());
            }
            let data = cli.run(&argv, true).await?;
            let Some(rows) = data.get("messages").and_then(array_rows).or_else(|| {
                // A missing key is an empty page. A non-array is an error.
                if data.get("messages").is_none() || data["messages"].is_null() {
                    Some(Vec::new())
                } else {
                    None
                }
            }) else {
                return Err(HyphaeCliError::Failed(
                    "groupchat history messages is not an array".to_owned(),
                ));
            };
            let len = rows.len();
            let mut advanced: Option<String> = None;
            for (index, row) in rows.iter().enumerate() {
                match self
                    .ingest_group(npub, locals, nickname, group_id, row)
                    .await
                {
                    Handled::Done { logical_id } => advanced = Some(logical_id),
                    Handled::Stop { logical_id, reason } => {
                        let key = logical_id.clone().unwrap_or_else(|| format!("idx:{index}"));
                        let rounds = self.note_stuck(npub, group_id, &key);
                        self.quiet_warn(
                            &format!("stuck:{group_id}"),
                            &format!("group {group_id} row {key} stuck ({reason})"),
                        );
                        if rounds >= STUCK_ROUNDS {
                            self.record_filtered(npub, group_id, row, logical_id.as_deref(), index)
                                .await;
                            if let Some(logical_id) = logical_id {
                                advanced = Some(logical_id);
                            }
                            continue;
                        }
                        break;
                    }
                }
            }
            if let Some(logical_id) = advanced.as_deref()
                && logical_id != cursor.as_deref().unwrap_or("")
            {
                self.store
                    .set_inbound_cursor(SOURCE, npub, group_id, logical_id)
                    .await
                    .map_err(|err| HyphaeCliError::Failed(format!("store: {err}")))?;
            }
            if (len as u32) < HISTORY_LIMIT || advanced.is_none() {
                break;
            }
        }
        Ok(())
    }

    async fn ingest_direct(&self, npub: &str, nickname: &str, row: &Value) {
        // Own messages are filtered by the caller, which sees every local npub.
        let Some(event_id) = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        let Some(actor) = row
            .get("sender_npub")
            .and_then(Value::as_str)
            .filter(|actor| !actor.is_empty())
        else {
            return;
        };
        let Some(at) = created_at(row) else {
            return;
        };
        let text = body_text(row);
        let protocol = is_protocol(&text);
        let allowlisted = self.allowlisted(actor).await;
        let category = if protocol {
            "protocol"
        } else if allowlisted {
            "direct"
        } else {
            "stranger"
        };
        self.ingest(InboundMessage {
            id: format!("hyphae:{event_id}"),
            source: SOURCE.to_owned(),
            category: category.to_owned(),
            recipient_identity: Some(npub.to_owned()),
            thread_key: None,
            actor: Some(actor.to_owned()),
            human: !protocol,
            addressed: "direct".to_owned(),
            at,
            text: Some(text),
            meta: Some(format!("{{\"nickname\":{}}}", json_string(nickname))),
        })
        .await;
    }

    async fn ingest_group(
        &self,
        npub: &str,
        locals: &HashSet<String>,
        nickname: &str,
        group_id: &str,
        row: &Value,
    ) -> Handled {
        let Some(logical_id) = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return Handled::Stop {
                logical_id: None,
                reason: "missing logical id",
            };
        };
        let logical_id = logical_id.to_owned();
        if own_group(row, locals) {
            return Handled::Done { logical_id };
        }
        let Some(event_id) = row
            .get("event_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            // Incoming row with no event id cannot be deduped. Do not skip
            // past it until it has been stuck for STUCK_ROUNDS.
            return Handled::Stop {
                logical_id: Some(logical_id),
                reason: "missing event id",
            };
        };
        let Some(actor) = row
            .get("sender")
            .and_then(Value::as_str)
            .filter(|actor| !actor.is_empty())
        else {
            return Handled::Stop {
                logical_id: Some(logical_id),
                reason: "missing sender",
            };
        };
        let Some(at) = created_at(row) else {
            return Handled::Stop {
                logical_id: Some(logical_id),
                reason: "missing created_at",
            };
        };
        let text = body_text(row);
        let protocol = is_protocol(&text);
        let mentioned = mentions(&text, npub, nickname);
        let allowlisted = self.allowlisted(actor).await;
        let (category, addressed) = if protocol {
            ("protocol", if mentioned { "mention" } else { "none" })
        } else if !allowlisted {
            ("stranger", if mentioned { "mention" } else { "none" })
        } else if mentioned {
            ("group_mention", "mention")
        } else {
            ("group", "none")
        };
        let thread = row
            .get("group_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .unwrap_or(group_id);
        self.ingest(InboundMessage {
            id: format!("hyphae:{event_id}"),
            source: SOURCE.to_owned(),
            category: category.to_owned(),
            recipient_identity: Some(npub.to_owned()),
            thread_key: Some(thread.to_owned()),
            actor: Some(actor.to_owned()),
            human: !protocol,
            addressed: addressed.to_owned(),
            at,
            text: Some(text),
            meta: None,
        })
        .await;
        Handled::Done { logical_id }
    }

    async fn allowlisted(&self, actor: &str) -> bool {
        match self.store.inbound_actor_allowlisted(SOURCE, actor).await {
            Ok(yes) => yes,
            Err(err) => {
                self.quiet_warn("allowlist", &format!("allowlist lookup failed: {err}"));
                false
            }
        }
    }

    async fn ingest(&self, message: InboundMessage) {
        if let Err(err) = self.service.ingest(message).await {
            self.quiet_warn("ingest", &format!("ingest failed: {err:?}"));
        }
    }

    /// Leave a `disposition=filtered` row and let the cursor move past.
    async fn record_filtered(
        &self,
        npub: &str,
        group_id: &str,
        row: &Value,
        logical_id: Option<&str>,
        index: usize,
    ) {
        let stamp = format_inbound_timestamp((self.now)());
        let at = created_at(row).unwrap_or_else(|| stamp.clone());
        let id = match logical_id {
            Some(logical_id) => format!("hyphae:filtered:{npub}:{group_id}:{logical_id}"),
            None => format!("hyphae:filtered:{npub}:{group_id}:idx{index}"),
        };
        let actor = row
            .get("sender")
            .and_then(Value::as_str)
            .filter(|actor| !actor.is_empty())
            .map(str::to_owned);
        let text = body_text(row);
        let event = agent24_store::NewInboundEvent {
            id,
            source: SOURCE.to_owned(),
            category: "group".to_owned(),
            recipient_identity: Some(npub.to_owned()),
            thread_key: Some(group_id.to_owned()),
            actor,
            human: false,
            addressed: "none".to_owned(),
            at,
            received_at: stamp,
            text: (!text.is_empty()).then_some(text),
            meta: None,
            disposition: "filtered".to_owned(),
            disposition_reason: Some("missing_field".to_owned()),
            policy_version: None,
            run_state: None,
        };
        if let Err(err) = self.store.insert_inbound_event(&event).await {
            self.quiet_warn("store", &format!("group {group_id} filtered row: {err}"));
        }
    }

    /// Rounds this `(npub, group)` has stopped on `key`, including this one.
    fn note_stuck(&self, npub: &str, group_id: &str, key: &str) -> u32 {
        let mut stuck = self
            .stuck
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = stuck
            .entry((npub.to_owned(), group_id.to_owned()))
            .or_insert_with(|| (key.to_owned(), 0));
        if slot.0 != key {
            *slot = (key.to_owned(), 0);
        }
        slot.1 = slot.1.saturating_add(1);
        slot.1
    }

    fn note_cli(&self, err: HyphaeCliError) {
        let (category, message) = match &err {
            HyphaeCliError::NotInstalled => ("cli-missing", err.text()),
            HyphaeCliError::Locked => ("cli-locked", err.text()),
            HyphaeCliError::Failed(_) => ("cli-failed", err.text()),
        };
        self.quiet_warn(category, &message);
    }

    fn count_direct_attempt(&self) {
        self.direct_attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn quiet_warn(&self, category: &str, message: &str) {
        let now = std::time::Instant::now();
        let mut slots = self
            .warned_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !take_warn_slot(&mut slots, category, now, QUIET_WARN_INTERVAL) {
            return;
        }
        tracing::warn!(target: "hyphae_inbound", category, "{message}");
    }
}

enum Handled {
    Done {
        logical_id: String,
    },
    Stop {
        logical_id: Option<String>,
        reason: &'static str,
    },
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

fn args_limit(parts: &[&str]) -> Vec<String> {
    let mut argv = args(parts);
    argv.push("--limit".to_owned());
    argv.push(HISTORY_LIMIT.to_string());
    argv
}

fn identity_rows(data: &Value) -> Option<Vec<(String, String)>> {
    let rows = array_rows(data)?;
    let mut out = Vec::new();
    for row in rows {
        let nickname = row.get("nickname").and_then(Value::as_str)?;
        let npub = row.get("npub").and_then(Value::as_str)?;
        if nickname.is_empty() || npub.is_empty() {
            continue;
        }
        out.push((nickname.to_owned(), npub.to_owned()));
    }
    Some(out)
}

/// Groups whose `state` is `active`. Left and not-yet-active rows stay in
/// `groupchat list` (`commands_list.go`) and their history is
/// `ErrGroupLeft` / `ErrGroupNotActive`. A missing state is not active.
fn active_group_ids(data: &Value) -> Option<Vec<String>> {
    let rows = data.get("groups").and_then(array_rows).or_else(|| {
        if data.get("groups").is_none() || data["groups"].is_null() {
            Some(Vec::new())
        } else {
            None
        }
    })?;
    let mut out = Vec::new();
    for row in rows {
        if row.get("state").and_then(Value::as_str) != Some("active") {
            continue;
        }
        if let Some(id) = row
            .get("group_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            out.push(id.to_owned());
        }
    }
    Some(out)
}

/// JSON array, or JSON null / missing (Go nil slice) as an empty list.
/// A non-array value is `None`.
fn array_rows(data: &Value) -> Option<Vec<&Value>> {
    if data.is_null() {
        return Some(Vec::new());
    }
    data.as_array().map(|rows| rows.iter().collect())
}

fn is_local(sender: Option<&str>, locals: &HashSet<String>) -> bool {
    sender.is_some_and(|sender| locals.contains(sender))
}

/// Every npub from `identity list` is us. A message A sent is not inbound
/// on B's poll when both are local. An outgoing copy (`is_incoming` false)
/// is not inbound either.
fn own_direct(row: &Value, locals: &HashSet<String>) -> bool {
    is_local(row.get("sender_npub").and_then(Value::as_str), locals)
        || row.get("is_incoming").and_then(Value::as_bool) == Some(false)
}

fn own_group(row: &Value, locals: &HashSet<String>) -> bool {
    is_local(row.get("sender").and_then(Value::as_str), locals)
}

fn body_text(row: &Value) -> String {
    let plain = row.get("plaintext").and_then(Value::as_str).unwrap_or("");
    if !plain.is_empty() {
        return plain.to_owned();
    }
    row.get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// Machine protocol. See the module comment. Not a model call.
fn is_protocol(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.starts_with(CANARY_PREFIX)
        || trimmed.starts_with(FRIEND_PREFIX)
        || trimmed.starts_with(GROUP_PREFIX)
    {
        return true;
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("version").and_then(Value::as_str) == Some("f4/1") {
        return true;
    }
    object.get("protocol").and_then(Value::as_str) == Some("hyphae-behavior/1")
        && matches!(
            object.get("type").and_then(Value::as_str),
            Some("query" | "response")
        )
}

/// No mention field on the wire. The local npub as a substring, or
/// `@nickname` with a boundary on both sides. The character before `@`
/// is the start of the text, whitespace, or punctuation. `bob@me.com`
/// is an address, not a mention.
fn mentions(text: &str, npub: &str, nickname: &str) -> bool {
    if !npub.is_empty() && text.contains(npub) {
        return true;
    }
    if nickname.is_empty() {
        return false;
    }
    let needle = format!("@{nickname}");
    text.match_indices(&needle).any(|(index, _)| {
        let left = index == 0
            || text[..index]
                .chars()
                .next_back()
                .is_some_and(is_mention_boundary);
        let right = text[index + needle.len()..]
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
        left && right
    })
}

fn is_mention_boundary(ch: char) -> bool {
    ch.is_whitespace() || ch.is_ascii_punctuation() || is_cjk_punctuation(ch)
}

fn is_cjk_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '，' | '。'
            | '、'
            | '；'
            | '：'
            | '！'
            | '？'
            | '（'
            | '）'
            | '「'
            | '」'
            | '『'
            | '』'
            | '《'
            | '》'
            | '【'
            | '】'
            | '…'
            | '—'
            | '～'
            | '·'
    )
}

/// Inbox order is newest first. Missing `created_at` sorts last.
fn direct_order(row: &Value) -> (i64, &str) {
    let secs = row
        .get("created_at")
        .and_then(Value::as_i64)
        .unwrap_or(i64::MAX);
    let id = row.get("id").and_then(Value::as_str).unwrap_or("");
    (secs, id)
}

fn unknown_cursor(err: &HyphaeCliError) -> bool {
    match err {
        HyphaeCliError::Failed(message) => message.contains(UNKNOWN_CURSOR),
        _ => false,
    }
}

/// `true` when this category should emit a warning now.
fn take_warn_slot(
    slots: &mut HashMap<String, std::time::Instant>,
    category: &str,
    now: std::time::Instant,
    interval: Duration,
) -> bool {
    let due = slots
        .get(category)
        .map(|then| now.duration_since(*then) >= interval)
        .unwrap_or(true);
    if due {
        slots.insert(category.to_owned(), now);
    }
    due
}

fn created_at(row: &Value) -> Option<String> {
    let secs = match row.get("created_at")? {
        Value::Number(number) => number.as_i64().filter(|secs| *secs >= 0)?,
        Value::String(text) => {
            if let Ok(secs) = text.parse::<i64>() {
                if secs < 0 {
                    return None;
                }
                secs
            } else {
                return DateTime::parse_from_rfc3339(text)
                    .ok()
                    .map(|dt| format_inbound_timestamp(dt.with_timezone(&Utc)));
            }
        }
        _ => return None,
    };
    Utc.timestamp_opt(secs, 0)
        .single()
        .map(format_inbound_timestamp)
}

fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Mutex as StdMutex;

    use agent24_models::router::{ModelRouter, Tier};
    use agent24_models::{CompletionRequest, CompletionResponse, ModelError, ModelProvider, Msg};
    use async_trait::async_trait;
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    const FIXTURE: &str =
        include_str!("../../../../crates/agent24-comm/tests/fixtures/comm_inbound_six.json");
    const SELF_NPUB: &str = "npub1selflocalidentity000000000000000000000000000000000000000";
    const PEER: &str = "npub1peerallowlisted0000000000000000000000000000000000000000";
    const STRANGER: &str = "npub1strangeroutsidethelist000000000000000000000000000000000";
    const NICK: &str = "me";
    const GROUP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct Fixed;

    #[async_trait]
    impl ModelProvider for Fixed {
        fn name(&self) -> &str {
            "fixed"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            Ok(CompletionResponse {
                message: Msg::assistant(Some("pong".to_owned()), vec![]),
                usage: agent24_protocol::Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost_usd: 0.0,
                },
                model_id: None,
            })
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }

    struct Script {
        calls: StdMutex<Vec<(Vec<String>, bool)>>,
        directs: StdMutex<Vec<Value>>,
        groups: StdMutex<Vec<Value>>,
        fail: StdMutex<Option<HyphaeCliError>>,
    }

    #[async_trait]
    impl HyphaeCli for Script {
        async fn run(&self, args: &[String], with_password: bool) -> Result<Value, HyphaeCliError> {
            self.calls
                .lock()
                .unwrap()
                .push((args.to_vec(), with_password));
            if let Some(err) = self.fail.lock().unwrap().clone() {
                return Err(err);
            }
            if args.first().map(String::as_str) == Some("identity") {
                return Ok(json!([{ "nickname": NICK, "npub": SELF_NPUB, "default": true }]));
            }
            if args.first().map(String::as_str) == Some("history") {
                return Ok(Value::Array(self.directs.lock().unwrap().clone()));
            }
            if args.get(1).map(String::as_str) == Some("list") {
                return Ok(json!({ "groups": [{ "group_id": GROUP, "state": "active" }] }));
            }
            let after = args.windows(2).find(|pair| pair[0] == "--after-id");
            let messages = if after.is_some() {
                Vec::new()
            } else {
                self.groups.lock().unwrap().clone()
            };
            Ok(json!({ "messages": messages }))
        }
    }

    /// Whole-second wall clock. Admit compares `at` with the real `Utc::now`
    /// and a 600s max age, so a frozen historical stamp would be stale.
    fn wall_now() -> DateTime<Utc> {
        Utc.timestamp_opt(Utc::now().timestamp(), 0)
            .single()
            .unwrap()
    }

    fn direct(id: &str, sender: &str, created: i64, content: &str, incoming: bool) -> Value {
        json!({
            "id": id,
            "sender_npub": sender,
            "recipient_npub": SELF_NPUB,
            "content": content,
            "plaintext": content,
            "created_at": created,
            "received_at": created + 1,
            "is_encrypted": true,
            "is_incoming": incoming,
        })
    }

    fn group_msg(logical: &str, event_id: &str, sender: &str, created: i64, text: &str) -> Value {
        json!({
            "id": logical,
            "event_id": event_id,
            "group_id": GROUP,
            "sender": sender,
            "content": text,
            "plaintext": text,
            "created_at": created,
            "is_encrypted": true,
        })
    }

    async fn open(script: Arc<Script>, now: DateTime<Utc>) -> (HyphaeInbound, Store) {
        let store = Store::open_memory().await.unwrap();
        let runs = RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(
                Arc::new(Fixed) as Arc<dyn ModelProvider>,
                Tier::Local,
            )])),
            Arc::new(agent24_tools::ToolRegistry::new()),
            Arc::new(Noop),
            CancellationToken::new(),
        );
        let inbound = HyphaeInbound::new(
            store.clone(),
            runs,
            Arc::new({
                let script = Arc::clone(&script);
                move || Some(Arc::clone(&script) as Arc<dyn HyphaeCli>)
            }),
            Arc::new(move || now),
        );
        (inbound, store)
    }

    async fn observe(store: &Store, mode: &str) {
        store
            .set_inbound_policy(mode, &format_inbound_timestamp(Utc::now()))
            .await
            .unwrap();
    }

    struct Noop;
    impl agent24_agent::EventSink for Noop {
        fn emit(&self, _body: agent24_protocol::EventBody) {}
    }

    async fn allow(store: &Store, actor: &str) {
        sqlx::query("INSERT INTO inbound_allowlist (source, actor, added_at) VALUES (?, ?, ?)")
            .bind(SOURCE)
            .bind(actor)
            .bind(format_inbound_timestamp(Utc::now()))
            .execute(agent24_store::test_hooks::pool(store))
            .await
            .unwrap();
    }

    async fn runs(store: &Store) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(agent24_store::test_hooks::pool(store))
            .await
            .unwrap()
    }

    fn samples() -> Vec<Value> {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        fixture["samples"].as_array().unwrap().clone()
    }

    fn sample(category: &str) -> Value {
        samples()
            .into_iter()
            .find(|sample| sample["category"] == category)
            .unwrap()
    }

    async fn poll_directs(rows: Vec<Value>, now: DateTime<Utc>) -> (Store, Arc<Script>) {
        let script = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(rows),
            groups: StdMutex::new(Vec::new()),
            fail: StdMutex::new(None),
        });
        let (inbound, store) = open(Arc::clone(&script), now).await;
        allow(&store, PEER).await;
        inbound.poll_round().await.unwrap();
        (store, script)
    }

    /// Mutation: treat a missing policy row as opt-in and the first assert
    /// goes red. Drop the allowlist arm and the second goes red. Drop
    /// `observe`/`auto` and the third goes red. Treat `off` as opt-in and
    /// the fourth goes red.
    #[test]
    fn poll_requires_an_allowlist_row_or_an_explicit_mode() {
        assert!(!hyphae_poll_enabled(None, false), "no policy and no list");
        assert!(hyphae_poll_enabled(None, true), "hyphae allowlist");
        assert!(hyphae_poll_enabled(Some("observe"), false));
        assert!(hyphae_poll_enabled(Some("auto"), false));
        assert!(!hyphae_poll_enabled(Some("off"), false));
        // A hyphae allowlist row is an opt-in on its own, including when
        // the stored mode is off.
        assert!(
            !hyphae_poll_enabled(Some("off"), true),
            "off overrides the allowlist"
        );
    }

    #[tokio::test]
    async fn plain_sample_is_human_direct_and_starts_one_run() {
        let now_at = wall_now();
        let content = sample("plain")["content"].as_str().unwrap().to_owned();
        let (store, _) = poll_directs(
            vec![direct(
                "evt-plain",
                PEER,
                now_at.timestamp(),
                &content,
                true,
            )],
            now_at,
        )
        .await;
        let row = store
            .get_inbound_event("hyphae:evt-plain")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.source, "hyphae");
        assert_eq!(row.category, "direct");
        assert!(row.human);
        assert_eq!(row.addressed, "direct");
        assert_eq!(row.at, format_inbound_timestamp(now_at));
        assert_eq!(row.actor.as_deref(), Some(PEER));
        assert_eq!(row.disposition, "explicit");
        assert_eq!(runs(&store).await, 1);
    }

    async fn protocol_sample_is_ignore_and_starts_nothing(category: &str, id: &str) {
        let now_at = wall_now();
        let content = sample(category)["content"].as_str().unwrap().to_owned();
        let (store, _) = poll_directs(
            vec![direct(id, PEER, now_at.timestamp(), &content, true)],
            now_at,
        )
        .await;
        let row = store
            .get_inbound_event(&format!("hyphae:{id}"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.category, "protocol", "{category}");
        assert!(!row.human, "{category}");
        assert_eq!(row.disposition, "ignore", "{category}");
        assert_eq!(
            row.disposition_reason.as_deref(),
            Some("protocol"),
            "{category}"
        );
        assert_eq!(runs(&store).await, 0, "{category}");
    }

    // Mutation: delete `is_protocol`'s f4/1 arm and the kind-30078 and receipt
    // tests go red. Delete the hyphae-behavior arm and query + response go
    // red. Delete the canary prefix and the canary test goes red.
    #[tokio::test]
    async fn kind_30078_f4_envelope_is_protocol_zero_run() {
        protocol_sample_is_ignore_and_starts_nothing("kind-30078", "evt-f4").await;
    }

    #[tokio::test]
    async fn receipt_ack_is_protocol_zero_run() {
        protocol_sample_is_ignore_and_starts_nothing("receipt", "evt-ack").await;
    }

    #[tokio::test]
    async fn hyphae_behavior_query_is_protocol_zero_run() {
        protocol_sample_is_ignore_and_starts_nothing("query", "evt-query").await;
    }

    #[tokio::test]
    async fn hyphae_behavior_response_is_protocol_zero_run() {
        protocol_sample_is_ignore_and_starts_nothing("response", "evt-response").await;
    }

    #[tokio::test]
    async fn canary_is_protocol_zero_run() {
        protocol_sample_is_ignore_and_starts_nothing("canary", "evt-canary").await;
    }

    #[tokio::test]
    async fn friend_protocol_is_protocol_zero_run() {
        let now_at = wall_now();
        let body = "agent24.friend/v1\n{\"type\":\"request\"}";
        let (store, _) = poll_directs(
            vec![direct("evt-friend", PEER, now_at.timestamp(), body, true)],
            now_at,
        )
        .await;
        let row = store
            .get_inbound_event("hyphae:evt-friend")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.category, "protocol");
        assert_eq!(row.disposition_reason.as_deref(), Some("protocol"));
        assert_eq!(runs(&store).await, 0);
    }

    #[tokio::test]
    async fn group_mention_direct_and_stranger_categories() {
        let now_at = wall_now();
        let now = now_at.timestamp();
        let mention = format!("hello {SELF_NPUB}");
        let script = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(vec![direct("evt-stranger", STRANGER, now, "hi", true)]),
            groups: StdMutex::new(vec![
                group_msg("logical-mention", "evt-mention", PEER, now, &mention),
                group_msg("logical-plain", "evt-group", PEER, now, "no mention here"),
            ]),
            fail: StdMutex::new(None),
        });
        let (inbound, store) = open(Arc::clone(&script), now_at).await;
        allow(&store, PEER).await;
        inbound.poll_round().await.unwrap();

        let mention_row = store
            .get_inbound_event("hyphae:evt-mention")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mention_row.category, "group_mention");
        assert_eq!(mention_row.addressed, "mention");
        assert_eq!(mention_row.thread_key.as_deref(), Some(GROUP));
        assert_eq!(mention_row.disposition, "explicit");

        let group_row = store
            .get_inbound_event("hyphae:evt-group")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(group_row.category, "group");
        assert_eq!(group_row.addressed, "none");
        assert_eq!(group_row.disposition_reason.as_deref(), Some("no_rule"));

        let stranger = store
            .get_inbound_event("hyphae:evt-stranger")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stranger.category, "stranger");
        assert_eq!(stranger.addressed, "direct");
        assert_eq!(
            stranger.disposition_reason.as_deref(),
            Some("not_allowlisted")
        );
        // mention starts one run; the unaddressed group and the stranger do not.
        assert_eq!(runs(&store).await, 1);

        let calls = script.calls.lock().unwrap().clone();
        assert!(
            calls.iter().any(|(argv, password)| {
                argv[..2] == ["history", "inbox"]
                    && argv.iter().any(|arg| arg == "--as")
                    && argv.iter().any(|arg| arg == NICK)
                    && !password
            }),
            "DM pull is history inbox --as without a password: {calls:?}"
        );
        assert!(
            calls.iter().any(|(argv, password)| {
                argv.first().map(String::as_str) == Some("groupchat")
                    && argv.get(1).map(String::as_str) == Some("history")
                    && *password
            }),
            "group history passes the keystore password: {calls:?}"
        );
    }

    /// Mutation: drop the `own_direct` / `own_group` filters and this row appears.
    #[tokio::test]
    async fn messages_from_self_are_not_stored() {
        let now_at = wall_now();
        let now = now_at.timestamp();
        let script = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(vec![
                direct("evt-self", SELF_NPUB, now, "note to self", false),
                direct("evt-out", PEER, now, "outgoing copy", false),
            ]),
            groups: StdMutex::new(vec![group_msg(
                "logical-self",
                "evt-group-self",
                SELF_NPUB,
                now,
                "I said this",
            )]),
            fail: StdMutex::new(None),
        });
        let (inbound, store) = open(script, now_at).await;
        allow(&store, SELF_NPUB).await;
        allow(&store, PEER).await;
        inbound.poll_round().await.unwrap();
        assert!(
            store
                .get_inbound_event("hyphae:evt-self")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_inbound_event("hyphae:evt-out")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_inbound_event("hyphae:evt-group-self")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(runs(&store).await, 0);
    }

    /// Mutation: stop writing the activation watermark (pass a stamp an hour
    /// earlier). The 30s-old message is inside max age, so it starts a run
    /// and the `stale` / `0 runs` asserts go red. A missing row does not:
    /// ingest treats no watermark as universally stale.
    #[tokio::test]
    async fn first_activation_marks_earlier_history_stale_and_keeps_now() {
        let now_at = wall_now();
        let now = now_at.timestamp();
        let (store, _) = poll_directs(
            vec![
                direct("evt-old", PEER, now - 30, "before activation", true),
                direct("evt-now", PEER, now, "at activation", true),
            ],
            now_at,
        )
        .await;
        let old = store
            .get_inbound_event("hyphae:evt-old")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old.disposition_reason.as_deref(), Some("stale"));
        assert_eq!(
            old.at,
            format_inbound_timestamp(Utc.timestamp_opt(now - 30, 0).single().unwrap())
        );
        assert_ne!(old.at, old.received_at);
        let fresh = store
            .get_inbound_event("hyphae:evt-now")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fresh.disposition, "explicit");
        assert_eq!(fresh.at, format_inbound_timestamp(now_at));
        let activated = store
            .inbound_watermark_activated_at(SOURCE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(activated, format_inbound_timestamp(now_at));
        assert_eq!(runs(&store).await, 1);
    }

    #[tokio::test]
    async fn offline_backlog_older_than_max_age_is_stale() {
        let now_at = wall_now();
        let now = now_at.timestamp();
        let script = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(vec![direct(
                "evt-backlog",
                PEER,
                now - 11 * 60,
                "while we were down",
                true,
            )]),
            groups: StdMutex::new(Vec::new()),
            fail: StdMutex::new(None),
        });
        let (inbound, store) = open(script, now_at).await;
        allow(&store, PEER).await;
        // Already activated an hour ago. The message is after that watermark
        // and only the 10-minute max age marks it stale.
        store
            .ensure_inbound_watermark(
                SOURCE,
                &format_inbound_timestamp(Utc.timestamp_opt(now - 3600, 0).single().unwrap()),
                600,
            )
            .await
            .unwrap();
        inbound.poll_round().await.unwrap();
        let row = store
            .get_inbound_event("hyphae:evt-backlog")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.disposition_reason.as_deref(), Some("stale"));
        assert_eq!(runs(&store).await, 0);
        let activated = store
            .inbound_watermark_activated_at(SOURCE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            activated,
            format_inbound_timestamp(Utc.timestamp_opt(now - 3600, 0).single().unwrap()),
            "a later poll must not move the watermark"
        );
    }

    /// Mutation: stop persisting the group cursor. The second poll's
    /// `groupchat history` lacks `--after-id` and this assert goes red.
    #[tokio::test]
    async fn group_cursor_survives_a_new_poller_and_does_not_replay() {
        let now_at = wall_now();
        let now = now_at.timestamp();
        let script = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(Vec::new()),
            groups: StdMutex::new(vec![group_msg(
                "logical-one",
                "evt-once",
                PEER,
                now,
                &format!("@{NICK} hi"),
            )]),
            fail: StdMutex::new(None),
        });
        let (first, store) = open(Arc::clone(&script), now_at).await;
        allow(&store, PEER).await;
        first.poll_round().await.unwrap();
        assert_eq!(runs(&store).await, 1);
        assert_eq!(
            store
                .inbound_cursor(SOURCE, SELF_NPUB, GROUP)
                .await
                .unwrap()
                .as_deref(),
            Some("logical-one")
        );

        let (second, _) = open(Arc::clone(&script), now_at).await;
        // Same store: rebuild the poller the way a restart would, sharing
        // the script so the second history call is visible.
        let second = HyphaeInbound {
            store: store.clone(),
            service: second.service,
            cli: second.cli,
            now: second.now,
            warned_at: Mutex::new(HashMap::new()),
            stuck: Mutex::new(HashMap::new()),
            direct_attempts: std::sync::atomic::AtomicUsize::new(0),
        };
        second.poll_round().await.unwrap();
        assert_eq!(runs(&store).await, 1, "restart must not start another run");
        let calls = script.calls.lock().unwrap().clone();
        let history: Vec<_> = calls
            .iter()
            .filter(|(argv, _)| {
                argv.first().map(String::as_str) == Some("groupchat")
                    && argv.get(1).map(String::as_str) == Some("history")
            })
            .collect();
        assert!(history.len() >= 2, "{history:?}");
        assert!(
            history
                .last()
                .unwrap()
                .0
                .windows(2)
                .any(|pair| pair[0] == "--after-id" && pair[1] == "logical-one"),
            "second poll must pass the stored cursor: {history:?}"
        );
    }

    #[tokio::test]
    async fn locked_keystore_skips_without_inserting() {
        let script = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(Vec::new()),
            groups: StdMutex::new(Vec::new()),
            fail: StdMutex::new(Some(HyphaeCliError::Locked)),
        });
        let (inbound, store) = open(Arc::clone(&script), wall_now()).await;
        observe(&store, "observe").await;
        inbound.poll_round().await.unwrap();
        inbound.poll_round().await.unwrap();
        assert!(
            !script.calls.lock().unwrap().is_empty(),
            "an explicit observe mode still calls the CLI"
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbound_events")
            .fetch_one(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(runs(&store).await, 0);
    }

    #[tokio::test]
    async fn poll_loop_stops_when_cancelled() {
        let script = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(Vec::new()),
            groups: StdMutex::new(Vec::new()),
            fail: StdMutex::new(Some(HyphaeCliError::NotInstalled)),
        });
        let (inbound, _) = open(script, wall_now()).await;
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let handle = tokio::spawn(async move {
            inbound.run(Duration::from_secs(3600), stop).await;
        });
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("poll loop did not observe the stop token")
            .unwrap();
    }

    #[test]
    fn nickname_mention_needs_an_at_boundary() {
        assert!(mentions("hey @me", SELF_NPUB, "me"));
        assert!(!mentions("metal", SELF_NPUB, "me"));
        assert!(!mentions("@metal", SELF_NPUB, "me"));
        assert!(mentions(&format!("see {SELF_NPUB}"), SELF_NPUB, "me"));
        assert!(!mentions("hello", SELF_NPUB, "me"));
        assert!(!mentions("bob@me.com", SELF_NPUB, "me"));
        assert!(mentions("(@me)", SELF_NPUB, "me"));
        assert!(mentions("hi,@me", SELF_NPUB, "me"));
    }

    #[test]
    fn warn_categories_do_not_share_a_timer() {
        let mut slots = HashMap::new();
        let start = std::time::Instant::now();
        let interval = Duration::from_secs(600);
        assert!(take_warn_slot(&mut slots, "store", start, interval));
        assert!(
            !take_warn_slot(&mut slots, "store", start, interval),
            "the same category stays quiet"
        );
        assert!(
            take_warn_slot(&mut slots, "group:left", start, interval),
            "a different category still warns"
        );
        assert!(take_warn_slot(
            &mut slots,
            "store",
            start + interval,
            interval
        ));
    }

    /// Mutation: drop the `hyphae.group/` arm. This row becomes a human
    /// direct and starts a run.
    #[tokio::test]
    async fn group_envelope_is_protocol_zero_run() {
        let now_at = wall_now();
        let body = "hyphae.group/v1\n{\"type\":\"message\",\"id\":\"m1\"}";
        let (store, _) = poll_directs(
            vec![direct(
                "evt-group-env",
                PEER,
                now_at.timestamp(),
                body,
                true,
            )],
            now_at,
        )
        .await;
        let row = store
            .get_inbound_event("hyphae:evt-group-env")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.category, "protocol");
        assert!(!row.human);
        assert_eq!(row.disposition_reason.as_deref(), Some("protocol"));
        assert_eq!(runs(&store).await, 0);
        assert!(is_protocol("hyphae.group/not-a-version"));
        assert!(!is_protocol("see hyphae.group/v1 later"));
    }

    /// Mutation: pull when the policy row is missing and the allowlist is
    /// empty, and the first assert sees CLI calls. Ignore the allowlist and
    /// the second poll stays quiet. Treat `off` as opt-in and the last poll
    /// calls the CLI.
    #[tokio::test]
    async fn poll_stays_off_until_an_allowlist_row_or_an_explicit_mode() {
        let now_at = wall_now();
        let script = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(Vec::new()),
            groups: StdMutex::new(Vec::new()),
            fail: StdMutex::new(None),
        });
        let (inbound, store) = open(Arc::clone(&script), now_at).await;
        inbound.poll_round().await.unwrap();
        assert!(
            script.calls.lock().unwrap().is_empty(),
            "no policy row and an empty allowlist does not pull"
        );

        allow(&store, PEER).await;
        inbound.poll_round().await.unwrap();
        assert!(
            !script.calls.lock().unwrap().is_empty(),
            "a hyphae allowlist row pulls"
        );

        let auto = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(Vec::new()),
            groups: StdMutex::new(Vec::new()),
            fail: StdMutex::new(None),
        });
        let (auto_in, auto_store) = open(Arc::clone(&auto), now_at).await;
        observe(&auto_store, "auto").await;
        auto_in.poll_round().await.unwrap();
        assert!(!auto.calls.lock().unwrap().is_empty(), "accept_mode=auto");

        let off = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(Vec::new()),
            groups: StdMutex::new(Vec::new()),
            fail: StdMutex::new(None),
        });
        let (off_in, off_store) = open(Arc::clone(&off), now_at).await;
        observe(&off_store, "off").await;
        off_in.poll_round().await.unwrap();
        assert!(
            off.calls.lock().unwrap().is_empty(),
            "accept_mode=off with an empty allowlist does not pull"
        );
    }

    /// Mutation: decide the opt-in once at startup. Setting `off` after the
    /// first round would still pull.
    #[tokio::test]
    async fn turning_accept_mode_off_stops_the_next_round() {
        let now_at = wall_now();
        let script = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(vec![direct(
                "evt-live",
                PEER,
                now_at.timestamp(),
                "hello",
                true,
            )]),
            groups: StdMutex::new(Vec::new()),
            fail: StdMutex::new(None),
        });
        let (mut inbound, store) = open(Arc::clone(&script), now_at).await;
        observe(&store, "observe").await;
        inbound.poll_round().await.unwrap();
        let pulled = script.calls.lock().unwrap().len();
        assert!(pulled > 0);
        assert!(
            store
                .get_inbound_event("hyphae:evt-live")
                .await
                .unwrap()
                .is_some()
        );

        observe(&store, "off").await;
        inbound.poll_round().await.unwrap();
        assert_eq!(
            script.calls.lock().unwrap().len(),
            pulled,
            "the next round must not call hyphae"
        );

        // The CLI handle is read again each round, so a binary that appears
        // after startup is used once the mode opts in.
        let later = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(Vec::new()),
            groups: StdMutex::new(Vec::new()),
            fail: StdMutex::new(None),
        });
        let cell = Arc::new(StdMutex::new(None::<Arc<Script>>));
        let cell_for_cli = Arc::clone(&cell);
        inbound.cli = Arc::new(move || {
            cell_for_cli
                .lock()
                .unwrap()
                .as_ref()
                .map(|script| Arc::clone(script) as Arc<dyn HyphaeCli>)
        });
        observe(&store, "observe").await;
        inbound.poll_round().await.unwrap();
        assert!(
            later.calls.lock().unwrap().is_empty(),
            "no CLI in the slot means no pull"
        );
        *cell.lock().unwrap() = Some(Arc::clone(&later));
        inbound.poll_round().await.unwrap();
        assert!(
            !later.calls.lock().unwrap().is_empty(),
            "a CLI configured after startup is picked up on the next round"
        );
    }

    /// Mutation: ingest the inbox in wire order (newest first). The first
    /// run's prompt is then "second". Drop the existing-id skip and the
    /// second round bumps `direct_attempts`.
    #[tokio::test]
    async fn direct_pages_run_oldest_first_and_skip_ids_already_stored() {
        let now_at = wall_now();
        let now = now_at.timestamp();
        let script = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(vec![
                direct("evt-new", PEER, now + 1, "second", true),
                direct("evt-old", PEER, now, "first", true),
            ]),
            groups: StdMutex::new(Vec::new()),
            fail: StdMutex::new(None),
        });
        let (inbound, store) = open(script, now_at).await;
        allow(&store, PEER).await;
        inbound.poll_round().await.unwrap();
        let prompts: Vec<String> = sqlx::query_scalar(
            "SELECT json_extract(input, '$.prompt') FROM runs ORDER BY created_at, id",
        )
        .fetch_all(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
        assert_eq!(
            prompts.first().map(String::as_str),
            Some("first"),
            "{prompts:?}"
        );
        let older = store
            .get_inbound_event("hyphae:evt-old")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(older.disposition, "explicit");
        let attempts = inbound
            .direct_attempts
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(attempts, 2);
        inbound.poll_round().await.unwrap();
        assert_eq!(
            inbound
                .direct_attempts
                .load(std::sync::atomic::Ordering::Relaxed),
            attempts,
            "a repeated inbox page must not ingest ids already stored"
        );
    }

    /// Mutation: treat only the npub currently being polled as self. B then
    /// stores A's message. Drop the identity from the cursor key and both
    /// cursors read back as `logical-b`.
    #[tokio::test]
    async fn every_local_npub_is_self_and_cursors_stay_per_identity() {
        let now_at = wall_now();
        let cli = Arc::new(TwoIdentities {
            now: now_at.timestamp(),
        });
        let (inbound, store) = open_cli(Arc::clone(&cli) as Arc<dyn HyphaeCli>, now_at).await;
        allow(&store, PEER).await;
        inbound.poll_round().await.unwrap();

        assert!(
            store
                .get_inbound_event("hyphae:evt-a-to-b")
                .await
                .unwrap()
                .is_none(),
            "A's DM is not inbound on B"
        );
        assert!(
            store
                .get_inbound_event("hyphae:evt-from-ann")
                .await
                .unwrap()
                .is_none(),
            "A's group message is not inbound on B"
        );
        assert!(
            store
                .get_inbound_event("hyphae:evt-peer-b")
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store
                .inbound_cursor(SOURCE, NPUB_A, GROUP)
                .await
                .unwrap()
                .as_deref(),
            Some("logical-a")
        );
        assert_eq!(
            store
                .inbound_cursor(SOURCE, NPUB_B, GROUP)
                .await
                .unwrap()
                .as_deref(),
            Some("logical-b")
        );
    }

    /// Mutation: `?` out of the group loop on the first error. The active
    /// group behind a left group (and behind a failing active group) is
    /// never fetched.
    #[tokio::test]
    async fn a_left_group_does_not_block_later_groups() {
        let now_at = wall_now();
        let cli = Arc::new(MixedGroups {
            calls: StdMutex::new(Vec::new()),
            now: now_at.timestamp(),
        });
        let (inbound, store) = open_cli(Arc::clone(&cli) as Arc<dyn HyphaeCli>, now_at).await;
        allow(&store, PEER).await;
        inbound.poll_round().await.unwrap();
        assert!(
            store
                .get_inbound_event("hyphae:evt-good")
                .await
                .unwrap()
                .is_some(),
            "the active group after a left group is ingested"
        );
        let calls = cli.calls.lock().unwrap().clone();
        let history: Vec<_> = calls
            .iter()
            .filter(|argv| argv.get(1).map(String::as_str) == Some("history"))
            .collect();
        assert!(
            history
                .iter()
                .any(|argv| argv.iter().any(|arg| arg == GOOD)),
            "{history:?}"
        );
        assert!(
            history
                .iter()
                .all(|argv| !argv.iter().any(|arg| arg == LEFT)),
            "left groups are not polled: {history:?}"
        );
        assert!(
            history.iter().any(|argv| argv.iter().any(|arg| arg == BAD)),
            "the failing active group is attempted, then the loop continues"
        );
    }

    /// Mutation: keep the unknown cursor. The retry never runs, so the fresh
    /// message is missing and this assert goes red. Clearing the watermark
    /// as well makes the fresh row `stale`.
    #[tokio::test]
    async fn an_unknown_cursor_is_cleared_and_history_recovers() {
        let now_at = wall_now();
        let now = now_at.timestamp();
        let cli = Arc::new(UnknownCursor { now });
        let (inbound, store) = open_cli(Arc::clone(&cli) as Arc<dyn HyphaeCli>, now_at).await;
        allow(&store, PEER).await;
        let activated =
            format_inbound_timestamp(Utc.timestamp_opt(now - 3600, 0).single().unwrap());
        store
            .ensure_inbound_watermark(SOURCE, &activated, 600)
            .await
            .unwrap();
        store
            .set_inbound_cursor(SOURCE, SELF_NPUB, GROUP, "not-a-message")
            .await
            .unwrap();
        inbound.poll_round().await.unwrap();

        let old = store
            .get_inbound_event("hyphae:evt-old-group")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old.disposition_reason.as_deref(), Some("stale"));
        let fresh = store
            .get_inbound_event("hyphae:evt-fresh-group")
            .await
            .unwrap()
            .unwrap();
        assert_ne!(fresh.disposition_reason.as_deref(), Some("stale"));
        assert_eq!(
            store
                .inbound_watermark_activated_at(SOURCE)
                .await
                .unwrap()
                .as_deref(),
            Some(activated.as_str()),
            "clearing the cursor must leave the watermark"
        );
        assert_eq!(
            store
                .inbound_cursor(SOURCE, SELF_NPUB, GROUP)
                .await
                .unwrap()
                .as_deref(),
            Some("logical-fresh")
        );
    }

    /// Mutation: keep stopping on the bad row. After three rounds the
    /// following message is still absent and there is no filtered trace.
    #[tokio::test]
    async fn a_row_stuck_three_rounds_is_skipped_and_filtered() {
        let now_at = wall_now();
        let now = now_at.timestamp();
        let script = Arc::new(Script {
            calls: StdMutex::new(Vec::new()),
            directs: StdMutex::new(Vec::new()),
            groups: StdMutex::new(vec![
                group_msg("logical-bad", "", PEER, now, "unreadable"),
                group_msg("logical-good", "evt-good", PEER, now, "readable"),
            ]),
            fail: StdMutex::new(None),
        });
        let (inbound, store) = open(script, now_at).await;
        allow(&store, PEER).await;
        inbound.poll_round().await.unwrap();
        inbound.poll_round().await.unwrap();
        assert!(
            store
                .get_inbound_event("hyphae:evt-good")
                .await
                .unwrap()
                .is_none()
        );
        inbound.poll_round().await.unwrap();
        assert!(
            store
                .get_inbound_event("hyphae:evt-good")
                .await
                .unwrap()
                .is_some()
        );
        let filtered = store
            .get_inbound_event(&format!("hyphae:filtered:{SELF_NPUB}:{GROUP}:logical-bad"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(filtered.disposition, "filtered");
        assert_eq!(
            filtered.disposition_reason.as_deref(),
            Some("missing_field")
        );
        assert_eq!(filtered.thread_key.as_deref(), Some(GROUP));
        assert_eq!(
            runs(&store).await,
            0,
            "the skipped row does not start a run"
        );
    }

    const NPUB_A: &str = "npub1localann000000000000000000000000000000000000000000000";
    const NPUB_B: &str = "npub1localbob000000000000000000000000000000000000000000000";
    const LEFT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const BAD: &str = "cccccccccccccccccccccccccccccccc";
    const GOOD: &str = "dddddddddddddddddddddddddddddddd";

    struct TwoIdentities {
        now: i64,
    }

    #[async_trait]
    impl HyphaeCli for TwoIdentities {
        async fn run(
            &self,
            args: &[String],
            _with_password: bool,
        ) -> Result<Value, HyphaeCliError> {
            if args.first().map(String::as_str) == Some("identity") {
                return Ok(json!([
                    { "nickname": "ann", "npub": NPUB_A },
                    { "nickname": "bob", "npub": NPUB_B }
                ]));
            }
            if args.first().map(String::as_str) == Some("history") {
                let whom = args
                    .windows(2)
                    .find(|pair| pair[0] == "--as")
                    .map(|pair| pair[1].as_str());
                if whom == Some("bob") {
                    return Ok(json!([
                        {
                            "id": "evt-a-to-b",
                            "sender_npub": NPUB_A,
                            "recipient_npub": NPUB_B,
                            "content": "from ann",
                            "plaintext": "from ann",
                            "created_at": self.now,
                            "is_incoming": true,
                        },
                        {
                            "id": "evt-peer-b",
                            "sender_npub": PEER,
                            "recipient_npub": NPUB_B,
                            "content": "from peer",
                            "plaintext": "from peer",
                            "created_at": self.now,
                            "is_incoming": true,
                        }
                    ]));
                }
                return Ok(json!([]));
            }
            if args.get(1).map(String::as_str) == Some("list") {
                return Ok(json!({ "groups": [{ "group_id": GROUP, "state": "active" }] }));
            }
            if args.windows(2).any(|pair| pair[0] == "--after-id") {
                return Ok(json!({ "messages": [] }));
            }
            let identity = args
                .windows(2)
                .find(|pair| pair[0] == "--identity")
                .map(|pair| pair[1].as_str());
            if identity == Some("ann") {
                return Ok(json!({
                    "messages": [{
                        "id": "logical-a",
                        "event_id": "evt-peer-a",
                        "group_id": GROUP,
                        "sender": PEER,
                        "content": "for ann",
                        "plaintext": "for ann",
                        "created_at": self.now,
                    }]
                }));
            }
            Ok(json!({
                "messages": [
                    {
                        "id": "logical-from-ann",
                        "event_id": "evt-from-ann",
                        "group_id": GROUP,
                        "sender": NPUB_A,
                        "content": "ann said",
                        "plaintext": "ann said",
                        "created_at": self.now - 1,
                    },
                    {
                        "id": "logical-b",
                        "event_id": "evt-peer-b-group",
                        "group_id": GROUP,
                        "sender": PEER,
                        "content": "for bob",
                        "plaintext": "for bob",
                        "created_at": self.now,
                    }
                ]
            }))
        }
    }

    struct MixedGroups {
        calls: StdMutex<Vec<Vec<String>>>,
        now: i64,
    }

    #[async_trait]
    impl HyphaeCli for MixedGroups {
        async fn run(
            &self,
            args: &[String],
            _with_password: bool,
        ) -> Result<Value, HyphaeCliError> {
            self.calls.lock().unwrap().push(args.to_vec());
            if args.first().map(String::as_str) == Some("identity") {
                return Ok(json!([{ "nickname": NICK, "npub": SELF_NPUB }]));
            }
            if args.first().map(String::as_str) == Some("history") {
                return Ok(json!([]));
            }
            if args.get(1).map(String::as_str) == Some("list") {
                return Ok(json!({
                    "groups": [
                        { "group_id": LEFT, "state": "left" },
                        { "group_id": BAD, "state": "active" },
                        { "group_id": GOOD, "state": "active" }
                    ]
                }));
            }
            let group = args
                .windows(2)
                .find(|pair| pair[0] == "--group-id")
                .map(|pair| pair[1].as_str());
            if group == Some(BAD) {
                return Err(HyphaeCliError::Failed("group is not active".to_owned()));
            }
            if group == Some(LEFT) {
                return Err(HyphaeCliError::Failed("group was left locally".to_owned()));
            }
            Ok(json!({
                "messages": [{
                    "id": "logical-good",
                    "event_id": "evt-good",
                    "group_id": GOOD,
                    "sender": PEER,
                    "content": "still here",
                    "plaintext": "still here",
                    "created_at": self.now,
                }]
            }))
        }
    }

    struct UnknownCursor {
        now: i64,
    }

    #[async_trait]
    impl HyphaeCli for UnknownCursor {
        async fn run(
            &self,
            args: &[String],
            _with_password: bool,
        ) -> Result<Value, HyphaeCliError> {
            if args.first().map(String::as_str) == Some("identity") {
                return Ok(json!([{ "nickname": NICK, "npub": SELF_NPUB }]));
            }
            if args.first().map(String::as_str) == Some("history") {
                return Ok(json!([]));
            }
            if args.get(1).map(String::as_str) == Some("list") {
                return Ok(json!({ "groups": [{ "group_id": GROUP, "state": "active" }] }));
            }
            if args.windows(2).any(|pair| pair[0] == "--after-id") {
                return Err(HyphaeCliError::Failed(format!(
                    "user_error: {UNKNOWN_CURSOR}"
                )));
            }
            Ok(json!({
                "messages": [
                    {
                        "id": "logical-old",
                        "event_id": "evt-old-group",
                        "group_id": GROUP,
                        "sender": PEER,
                        "content": "before the watermark",
                        "plaintext": "before the watermark",
                        "created_at": self.now - 7200,
                    },
                    {
                        "id": "logical-fresh",
                        "event_id": "evt-fresh-group",
                        "group_id": GROUP,
                        "sender": PEER,
                        "content": "after the watermark",
                        "plaintext": "after the watermark",
                        "created_at": self.now,
                    }
                ]
            }))
        }
    }

    async fn open_cli(cli: Arc<dyn HyphaeCli>, now: DateTime<Utc>) -> (HyphaeInbound, Store) {
        let store = Store::open_memory().await.unwrap();
        let runs = RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(
                Arc::new(Fixed) as Arc<dyn ModelProvider>,
                Tier::Local,
            )])),
            Arc::new(agent24_tools::ToolRegistry::new()),
            Arc::new(Noop),
            CancellationToken::new(),
        );
        let held = Arc::clone(&cli);
        let inbound = HyphaeInbound::new(
            store.clone(),
            runs,
            Arc::new(move || Some(Arc::clone(&held))),
            Arc::new(move || now),
        );
        (inbound, store)
    }
}
