//! K1-6a-G §2.5.1 (A5c): the ends of grants a module must be told of, with
//! their references, so the module can stop exactly their work and refuse
//! late calls carrying them. A row is written in the transaction of the
//! write that ended them: every revocation (each is told, by its route or
//! the notifier), and a decision replacing a granted row or an enable epoch
//! advance that ended grants not told yet. A crash before the notice, or a
//! later decision replacing the row, so loses none: every row not answered
//! nor settled stays pending for the notifier (A5c-2). A revocation also carries the
//! references of every end of its scope not confirmed stopped, so its answer
//! never claims a stop an earlier, unconfirmed notice left open (#1202 B1);
//! confirmed, it settles them, so no later one carries them again (G7).

use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{ConsentDecision, ModuleConsentRecord, decision_str, parse_instant, row_to_record};
use crate::{Result, Store, StoreError};

type Tx<'c> = sqlx::Transaction<'c, sqlx::Sqlite>;

/// How long a revocation's notice is left to the route that wrote it, which
/// waits for the module 10 s at most: the notifier sends it only past this.
/// A revocation no route tells (a disable's, a stop's, a package change's)
/// is not left: the module may run on (#1202 Low 2).
const ROUTE_GRACE_MILLIS: i64 = 30_000;

/// What ended the grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantEndReason {
    Revoked,
    /// A grant or refusal replaced the granted row.
    Replaced,
    /// The module's enable epoch advanced: its grants are `Stale`.
    Epoch,
}

impl GrantEndReason {
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::Replaced => "replaced",
            Self::Epoch => "epoch",
        }
    }

    fn parse(word: &str) -> Result<Self> {
        match word {
            "revoked" => Ok(Self::Revoked),
            "replaced" => Ok(Self::Replaced),
            "epoch" => Ok(Self::Epoch),
            other => Err(StoreError::Conflict(format!(
                "unknown grant end reason: {other}"
            ))),
        }
    }
}

/// One end of grants to tell a module of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantEndNotice {
    pub id: i64,
    pub module: String,
    /// The tool, or `*` for every tool of the module.
    pub op: String,
    /// The ended grants' references (`ga1:…`), sorted; empty when a
    /// revocation found none in force.
    pub authorization_refs: Vec<String>,
    pub reason: GrantEndReason,
    pub ended_at: String,
    /// The write that ended them: a revocation's is its own generation.
    pub generation: i64,
}

/// The granted rows of `module` (`op`: that tool's only; `epoch`: of that
/// enable epoch only), expired or not, whose end it was not told yet: no
/// revocation covers them, written being told (its row is), and no expiry
/// of them was told, but for one the module did not confirm stopped.
pub(super) async fn untold(
    tx: &mut Tx<'_>,
    module: &str,
    op: Option<&str>,
    epoch: Option<i64>,
) -> Result<Vec<ModuleConsentRecord>> {
    let rows = sqlx::query(
        "SELECT c.* FROM module_consents c
         WHERE c.module = ?1 AND (?2 IS NULL OR c.op = ?2)
           AND (?3 IS NULL OR c.enable_epoch = ?3)
           AND c.decision = ?4
           AND (c.expiry_stop_state IS NULL OR c.expiry_stop_state = 'unconfirmed')
           AND NOT EXISTS (SELECT 1 FROM module_consent_revocations r
               WHERE r.module = c.module AND r.op IN (c.op, '*')
                 AND r.generation >= c.generation)",
    )
    .bind(module)
    .bind(op)
    .bind(epoch)
    .bind(decision_str(ConsentDecision::Granted))
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(row_to_record).collect()
}

/// The references of `module`'s ends (`op`: that tool's and the whole
/// module's only) told but not confirmed stopped, or not told yet: a
/// revocation tells them again (#1202 B1), so its own answer covers them.
pub(super) async fn unconfirmed(
    tx: &mut Tx<'_>,
    module: &str,
    op: Option<&str>,
) -> Result<Vec<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT authorization_refs FROM module_grant_end_notices
         WHERE module = ?1 AND (?2 IS NULL OR op IN (?2, '*'))
           AND (stop_state IS NULL OR stop_state = 'unconfirmed')
           AND settled_by IS NULL",
    )
    .bind(module)
    .bind(op)
    .fetch_all(&mut **tx)
    .await?;
    let mut refs = Vec::new();
    for row in rows {
        refs.extend(parse_refs(&row)?);
    }
    Ok(refs)
}

/// Writes, in `tx`, that the write `generation` ended `ended`, and the
/// references `also`, for `module`'s `op` (`*`: every tool) at `ended_at`;
/// `routed` when a route tells it itself.
#[allow(clippy::too_many_arguments)]
pub(super) async fn write(
    tx: &mut Tx<'_>,
    module: &str,
    op: &str,
    ended: &[ModuleConsentRecord],
    also: &[String],
    reason: GrantEndReason,
    ended_at: &str,
    generation: i64,
    routed: bool,
) -> Result<()> {
    let mut refs: Vec<String> = ended
        .iter()
        .map(ModuleConsentRecord::authorization_ref)
        .chain(also.iter().cloned())
        .collect();
    refs.sort();
    refs.dedup();
    #[allow(
        clippy::expect_used,
        reason = "a Vec<String> always serializes to a JSON array of strings"
    )]
    let refs = serde_json::to_string(&refs).expect("references always serialize");
    sqlx::query(
        "INSERT INTO module_grant_end_notices
            (module, op, authorization_refs, reason, ended_at, generation, routed)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(module)
    .bind(op)
    .bind(refs)
    .bind(reason.word())
    .bind(ended_at)
    .bind(generation)
    .bind(routed)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn parse_refs(stored: &str) -> Result<Vec<String>> {
    serde_json::from_str(stored).map_err(|_| {
        StoreError::Conflict("a stored grant end's references are malformed".to_owned())
    })
}

fn row_to_notice(row: &SqliteRow) -> Result<GrantEndNotice> {
    let refs = parse_refs(&row.get::<String, _>("authorization_refs"))?;
    Ok(GrantEndNotice {
        id: row.get("id"),
        module: row.get("module"),
        op: row.get("op"),
        authorization_refs: refs,
        reason: GrantEndReason::parse(&row.get::<String, _>("reason"))?,
        ended_at: row.get("ended_at"),
        generation: row.get("generation"),
    })
}

impl Store {
    /// The ends not answered nor settled yet as of `now`, oldest first: a
    /// replacement's, an epoch's or a revocation no route tells at once, a
    /// routed revocation's once its route had its chance
    /// ([`ROUTE_GRACE_MILLIS`] after it ended).
    pub async fn pending_grant_end_notices(&self, now: &str) -> Result<Vec<GrantEndNotice>> {
        let (now, _) = parse_instant(now)?;
        let rows = sqlx::query(
            "SELECT * FROM module_grant_end_notices
             WHERE stop_state IS NULL AND settled_by IS NULL ORDER BY id",
        )
        .fetch_all(self.pool())
        .await?;
        let mut pending = Vec::new();
        for row in &rows {
            let notice = row_to_notice(row)?;
            let routes = row.get::<bool, _>("routed")
                && parse_instant(&notice.ended_at)
                    .is_ok_and(|(ended, _)| now < ended + ROUTE_GRACE_MILLIS);
            if !routes {
                pending.push(notice);
            }
        }
        Ok(pending)
    }

    /// The end the write `generation` wrote, if it wrote one: a
    /// revocation's always.
    pub async fn grant_end_notice(&self, generation: i64) -> Result<Option<GrantEndNotice>> {
        sqlx::query("SELECT * FROM module_grant_end_notices WHERE generation = ?")
            .bind(generation)
            .fetch_optional(self.pool())
            .await?
            .as_ref()
            .map(row_to_notice)
            .transpose()
    }

    /// Records what the module answered to end `id`; the first answer
    /// stands, so `false` when one was already recorded. A revocation's
    /// first answer `confirmed` settles, in the same transaction, the ends
    /// it carried not confirmed stopped (#1202 G7): the module's all, a
    /// tool's that tool's only, not the module's (`*`).
    pub async fn record_grant_end_stop(&self, id: i64, stop_state: &str) -> Result<bool> {
        let mut tx = self.pool().begin().await?;
        let recorded = sqlx::query(
            "UPDATE module_grant_end_notices SET stop_state = ?
             WHERE id = ? AND stop_state IS NULL
             RETURNING module, op, reason",
        )
        .bind(stop_state)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(recorded) = recorded else {
            return Ok(false);
        };
        let reason: String = recorded.get("reason");
        if stop_state == "confirmed" && reason == GrantEndReason::Revoked.word() {
            sqlx::query(
                "UPDATE module_grant_end_notices SET settled_by = ?1
                 WHERE module = ?2 AND (?3 = '*' OR op = ?3) AND id < ?1
                   AND (stop_state IS NULL OR stop_state = 'unconfirmed')
                   AND settled_by IS NULL",
            )
            .bind(id)
            .bind(recorded.get::<String, _>("module"))
            .bind(recorded.get::<String, _>("op"))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
