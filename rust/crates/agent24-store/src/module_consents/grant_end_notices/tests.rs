#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::{ConsentSource, HostRiskLevel, ToolPermissionSummary};

const AT: &str = "2026-10-25T00:00:00.000Z";
const UNTIL: &str = "2026-10-26T00:00:00.000Z";

fn summary(module: &str, op: &str) -> ToolPermissionSummary {
    ToolPermissionSummary::new(
        module,
        op,
        "1.0.0",
        ConsentSource::FirstParty,
        Some("docs".to_owned()),
        None,
        None,
        HostRiskLevel::Low,
    )
}

async fn grant(store: &Store, op: &str) -> ModuleConsentRecord {
    let summary = summary("m", op);
    store
        .grant_module_consent(&summary, AT, UNTIL)
        .await
        .unwrap()
}

async fn deny(store: &Store, op: &str) -> ModuleConsentRecord {
    let summary = summary("m", op);
    store
        .deny_module_consent(&summary, AT, UNTIL)
        .await
        .unwrap()
}

/// Every end written, oldest first.
async fn ends(store: &Store) -> Vec<GrantEndNotice> {
    let rows = sqlx::query("SELECT * FROM module_grant_end_notices ORDER BY id")
        .fetch_all(store.pool())
        .await
        .unwrap();
    rows.iter().map(|row| row_to_notice(row).unwrap()).collect()
}

fn refs(of: &[&ModuleConsentRecord]) -> Vec<String> {
    let mut refs: Vec<String> = of.iter().map(|r| r.authorization_ref()).collect();
    refs.sort();
    refs
}

/// K1-6a-G §2.5.1: a revocation writes the end of the grants in force it
/// ends, with their references, under its own generation: a tool's, then
/// the module's (another module's and a refusal are not its); one finding
/// none still writes one, empty.
#[tokio::test]
async fn a_revocation_tells_the_grants_it_ends() {
    let store = Store::open_memory().await.unwrap();
    let (t, u) = (grant(&store, "t").await, grant(&store, "u").await);
    deny(&store, "v").await;
    let elsewhere = summary("n", "t");
    store
        .grant_module_consent(&elsewhere, AT, UNTIL)
        .await
        .unwrap();
    let at = "2026-10-25T00:10:00Z";
    let tool = store
        .revoke_module_consent_tracked("m", Some("t"), at)
        .await
        .unwrap();
    let notice = store.grant_end_notice(tool).await.unwrap().unwrap();
    assert_eq!(
        (notice.op.as_str(), notice.reason, notice.generation),
        ("t", GrantEndReason::Revoked, tool)
    );
    assert_eq!(notice.authorization_refs, refs(&[&t]));
    assert_eq!(notice.ended_at, "2026-10-25T00:10:00.000Z");
    let module = store
        .revoke_module_consent_tracked("m", None, at)
        .await
        .unwrap();
    let notice = store.grant_end_notice(module).await.unwrap().unwrap();
    assert_eq!(notice.op, "*");
    assert_eq!(notice.authorization_refs, refs(&[&u]), "t's end was told");
    let again = store
        .revoke_module_consent_tracked("m", Some("t"), at)
        .await
        .unwrap();
    let notice = store.grant_end_notice(again).await.unwrap().unwrap();
    assert!(notice.authorization_refs.is_empty());
    assert_eq!(ends(&store).await.len(), 3);
}

/// `*` named as the tool revokes the whole module (its key): the end tells
/// every grant of it in force.
#[tokio::test]
async fn a_revocation_of_the_tool_named_star_tells_every_grant() {
    let store = Store::open_memory().await.unwrap();
    let (t, u) = (grant(&store, "t").await, grant(&store, "u").await);
    let all = store
        .revoke_module_consent_tracked("m", Some("*"), AT)
        .await
        .unwrap();
    let notice = store.grant_end_notice(all).await.unwrap().unwrap();
    assert_eq!(notice.op, "*");
    assert_eq!(notice.authorization_refs, refs(&[&t, &u]));
}

/// The stop route revokes the whole module, then advances its epoch: the
/// revocation's end tells every grant, and the advance, finding none left
/// untold, writes none.
#[tokio::test]
async fn an_advance_after_a_revocation_of_the_module_writes_no_second_end() {
    let store = Store::open_memory().await.unwrap();
    let t = grant(&store, "t").await;
    store.invalidate_module_consents("m").await.unwrap();
    store.advance_module_enable_epoch("m").await.unwrap();
    let told = ends(&store).await;
    assert_eq!(told.len(), 1);
    assert_eq!(told[0].reason, GrantEndReason::Revoked);
    assert_eq!(told[0].authorization_refs, refs(&[&t]));
}

/// A decision replacing a granted row writes its end, at the decision's
/// instant, under the decision's generation — an expired grant's too (the
/// A5b gate, #1190) — unless its end was told: by an expiry notice, or a
/// revocation. Replacing a refusal ends nothing.
#[tokio::test]
async fn a_decision_replacing_a_grant_tells_its_end() {
    let store = Store::open_memory().await.unwrap();
    let a = grant(&store, "t").await;
    let b = grant(&store, "t").await;
    let notice = store.grant_end_notice(b.generation).await.unwrap().unwrap();
    assert_eq!(
        (notice.op.as_str(), notice.reason, notice.ended_at.as_str()),
        ("t", GrantEndReason::Replaced, b.decided_at.as_str())
    );
    assert_eq!(notice.authorization_refs, refs(&[&a]));
    let d = deny(&store, "t").await;
    let notice = store.grant_end_notice(d.generation).await.unwrap().unwrap();
    assert_eq!(notice.authorization_refs, refs(&[&b]));
    let e = deny(&store, "t").await;
    assert_eq!(store.grant_end_notice(e.generation).await.unwrap(), None);
    let f = grant(&store, "t").await;
    assert_eq!(store.grant_end_notice(f.generation).await.unwrap(), None);
    // Expired, its notice not sent yet: replaced, it is told all the same.
    let summary = summary("m", "t");
    let expired = store
        .grant_module_consent(&summary, "2026-10-24T00:00:00Z", "2026-10-24T01:00:00Z")
        .await
        .unwrap();
    let g = grant(&store, "t").await;
    let notice = store.grant_end_notice(g.generation).await.unwrap().unwrap();
    assert_eq!(notice.authorization_refs, refs(&[&expired]));
    // Told by an expiry notice, or covered by a revocation: not again.
    store
        .record_expiry_stop("m", "t", g.generation, "confirmed")
        .await
        .unwrap();
    let h = grant(&store, "t").await;
    assert_eq!(store.grant_end_notice(h.generation).await.unwrap(), None);
    store
        .revoke_module_consent_tracked("m", Some("t"), AT)
        .await
        .unwrap();
    let i = grant(&store, "t").await;
    assert_eq!(store.grant_end_notice(i.generation).await.unwrap(), None);
}

/// An enable epoch advance writes the end of the module's grants of the
/// epoch it ends, for every tool, under the epoch's generation; those of
/// an earlier epoch were told when it ended. Seeing the module enabled
/// anew advances it, and tells them too.
#[tokio::test]
async fn an_epoch_advance_tells_the_grants_of_the_epoch_it_ends() {
    let store = Store::open_memory().await.unwrap();
    let t = grant(&store, "t").await;
    deny(&store, "u").await;
    let epoch = store.advance_module_enable_epoch("m").await.unwrap();
    let notice = store.grant_end_notice(epoch).await.unwrap().unwrap();
    assert_eq!(
        (notice.op.as_str(), notice.reason),
        ("*", GrantEndReason::Epoch)
    );
    assert_eq!(notice.authorization_refs, refs(&[&t]));
    let next = store.advance_module_enable_epoch("m").await.unwrap();
    assert_eq!(store.grant_end_notice(next).await.unwrap(), None);
    let u = grant(&store, "u").await;
    assert!(
        store
            .observe_module_enablement("m", true, "entry")
            .await
            .unwrap()
    );
    let told = ends(&store).await;
    assert_eq!(told.len(), 2);
    assert_eq!(told[1].authorization_refs, refs(&[&u]));
    assert!(
        !store
            .observe_module_enablement("m", true, "entry")
            .await
            .unwrap()
    );
    assert_eq!(ends(&store).await.len(), 2);
}

/// Ends are pending until answered, oldest first: a replacement's at once,
/// a revocation's only once its route had its chance; the first answer
/// stands.
#[tokio::test]
async fn ends_are_pending_until_answered_a_revocation_s_after_its_route() {
    let store = Store::open_memory().await.unwrap();
    grant(&store, "t").await;
    let at = "2026-10-25T00:10:00Z";
    let revoked = store
        .revoke_module_consent_tracked("m", Some("t"), at)
        .await
        .unwrap();
    grant(&store, "u").await;
    let replaced = grant(&store, "u").await.generation;
    let pending = |now: &'static str| {
        let store = store.clone();
        async move {
            let pending = store.pending_grant_end_notices(now).await.unwrap();
            pending.iter().map(|n| n.generation).collect::<Vec<_>>()
        }
    };
    assert_eq!(pending("2026-10-25T00:10:00Z").await, vec![replaced]);
    assert_eq!(pending("2026-10-25T00:10:29.999Z").await, vec![replaced]);
    assert_eq!(
        pending("2026-10-25T00:10:30Z").await,
        vec![revoked, replaced]
    );
    let id = store.grant_end_notice(revoked).await.unwrap().unwrap().id;
    assert!(store.record_grant_end_stop(id, "confirmed").await.unwrap());
    assert!(
        !store
            .record_grant_end_stop(id, "unconfirmed")
            .await
            .unwrap()
    );
    assert_eq!(pending("2026-10-25T00:10:30Z").await, vec![replaced]);
    assert!(store.pending_grant_end_notices("later").await.is_err());
}
