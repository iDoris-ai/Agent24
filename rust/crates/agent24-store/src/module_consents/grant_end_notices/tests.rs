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
    // Confirmed stopped: not told again (an unconfirmed one is, case 24).
    assert!(
        store
            .record_grant_end_stop(notice.id, GrantEndStop::Confirmed)
            .await
            .unwrap()
    );
    let module = store
        .revoke_module_consent_tracked("m", None, at)
        .await
        .unwrap();
    let notice = store.grant_end_notice(module).await.unwrap().unwrap();
    assert_eq!(notice.op, "*");
    assert_eq!(notice.authorization_refs, refs(&[&u]), "t's end was told");
    assert!(
        store
            .record_grant_end_stop(notice.id, GrantEndStop::NotRunning)
            .await
            .unwrap()
    );
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
/// a revocation's only once its route had its chance, but one no route
/// tells (a disable's, a stop's: #1202 Low 2) at once too; the first
/// answer stands.
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
    store
        .revoke_module_consent("m", Some("u"), at)
        .await
        .unwrap();
    let unrouted = ends(&store).await.pop().unwrap().generation;
    let pending = |now: &'static str| {
        let store = store.clone();
        async move {
            let pending = store.pending_grant_end_notices(now).await.unwrap();
            pending.iter().map(|n| n.generation).collect::<Vec<_>>()
        }
    };
    assert_eq!(
        pending("2026-10-25T00:10:00Z").await,
        vec![replaced, unrouted]
    );
    assert_eq!(
        pending("2026-10-25T00:10:29.999Z").await,
        vec![replaced, unrouted]
    );
    assert_eq!(
        pending("2026-10-25T00:10:30Z").await,
        vec![revoked, replaced, unrouted]
    );
    let id = store.grant_end_notice(revoked).await.unwrap().unwrap().id;
    assert!(
        store
            .record_grant_end_stop(id, GrantEndStop::Confirmed)
            .await
            .unwrap()
    );
    assert!(
        !store
            .record_grant_end_stop(id, GrantEndStop::Unconfirmed)
            .await
            .unwrap()
    );
    assert_eq!(
        pending("2026-10-25T00:10:30Z").await,
        vec![replaced, unrouted]
    );
    assert!(store.pending_grant_end_notices("later").await.is_err());
}

/// #1202 B1, case 24: a revocation tells again every end of its scope not
/// confirmed stopped, pending or unconfirmed, so its own answer covers
/// them; one confirmed or of another tool it does not.
#[tokio::test]
async fn a_revocation_tells_again_the_ends_not_confirmed_stopped() {
    let store = Store::open_memory().await.unwrap();
    let answered = |store: &Store, state: GrantEndStop| {
        let store = store.clone();
        async move {
            let last = ends(&store).await.pop().unwrap();
            assert!(store.record_grant_end_stop(last.id, state).await.unwrap());
        }
    };
    let a = grant(&store, "t").await;
    let b = grant(&store, "t").await;
    answered(&store, GrantEndStop::Unconfirmed).await;
    let c = grant(&store, "u").await;
    let u = grant(&store, "u").await;
    answered(&store, GrantEndStop::Confirmed).await;
    let e = grant(&store, "w").await;
    deny(&store, "w").await;
    let tool = store
        .revoke_module_consent_tracked("m", Some("t"), AT)
        .await
        .unwrap();
    assert_eq!(
        store
            .grant_end_notice(tool)
            .await
            .unwrap()
            .unwrap()
            .authorization_refs,
        refs(&[&a, &b]),
        "the tool's own and its replaced one, not w's"
    );
    let module = store
        .revoke_module_consent_tracked("m", None, AT)
        .await
        .unwrap();
    assert_eq!(
        store
            .grant_end_notice(module)
            .await
            .unwrap()
            .unwrap()
            .authorization_refs,
        refs(&[&a, &b, &e, &u]),
        "c was confirmed stopped"
    );
    let _ = c;
    // A tool's revocation carries the whole module's ends too: an epoch's
    // not confirmed stopped holds the tool's reference.
    let x = grant(&store, "x").await;
    store.advance_module_enable_epoch("m").await.unwrap();
    let epoch = ends(&store).await.pop().unwrap();
    assert_eq!(epoch.op, "*");
    let tool = store
        .revoke_module_consent_tracked("m", Some("y"), AT)
        .await
        .unwrap();
    assert!(
        store
            .grant_end_notice(tool)
            .await
            .unwrap()
            .unwrap()
            .authorization_refs
            .contains(&x.authorization_ref()),
        "the epoch's end, not confirmed, is carried by a tool's revocation"
    );
}

/// K1-6a-G §2.5.1 (#1202 G1): a grant is told by the end of its decision,
/// expired or not: expired and not yet told, a revocation tells it; told
/// expired but not confirmed stopped, a replacement or a revocation tells
/// it again; told and confirmed, it does not.
#[tokio::test]
async fn an_expired_grant_is_told_until_confirmed_stopped() {
    let store = Store::open_memory().await.unwrap();
    let lapsed = |op: &'static str| {
        let store = store.clone();
        async move {
            store
                .grant_module_consent(&summary("m", op), AT, "2026-10-25T00:00:01.000Z")
                .await
                .unwrap()
        }
    };
    let t = lapsed("t").await;
    let revoked = store
        .revoke_module_consent_tracked("m", Some("t"), "2026-10-25T00:10:00Z")
        .await
        .unwrap();
    assert_eq!(
        store
            .grant_end_notice(revoked)
            .await
            .unwrap()
            .unwrap()
            .authorization_refs,
        refs(&[&t])
    );
    let (u, v) = (lapsed("u").await, lapsed("v").await);
    store
        .record_expiry_stop("m", "u", u.generation, "unconfirmed")
        .await
        .unwrap();
    store
        .record_expiry_stop("m", "v", v.generation, "confirmed")
        .await
        .unwrap();
    let before = ends(&store).await.len();
    grant(&store, "u").await;
    grant(&store, "v").await;
    let after = ends(&store).await;
    assert_eq!(after.len(), before + 1, "v was confirmed stopped");
    assert_eq!(
        (after[before].op.as_str(), &after[before].authorization_refs),
        ("u", &refs(&[&u]))
    );
    // Case 26 (#1202 Low 1): told expired but not confirmed stopped, a
    // revocation tells it again too.
    let w = lapsed("w").await;
    store
        .record_expiry_stop("m", "w", w.generation, "unconfirmed")
        .await
        .unwrap();
    let revoked = store
        .revoke_module_consent_tracked("m", Some("w"), "2026-10-25T00:10:00Z")
        .await
        .unwrap();
    assert_eq!(carried(&store, revoked).await, refs(&[&w]));
}

/// Records `state` as the module's answer to the end the write
/// `generation` wrote.
async fn answer(store: &Store, generation: i64, state: GrantEndStop) {
    let end = store.grant_end_notice(generation).await.unwrap().unwrap();
    assert!(store.record_grant_end_stop(end.id, state).await.unwrap());
}

async fn carried(store: &Store, generation: i64) -> Vec<String> {
    store
        .grant_end_notice(generation)
        .await
        .unwrap()
        .unwrap()
        .authorization_refs
}

/// #1202 G7: a revocation confirmed stopped settles the ends it carried, so
/// lost notices do not pile up on every later one: after 100 cycles of an
/// unconfirmed revocation then a confirmed one, the next carries its own
/// reference only.
#[tokio::test]
async fn a_confirmed_revocation_settles_the_ends_it_carried() {
    let store = Store::open_memory().await.unwrap();
    for _ in 0..100 {
        grant(&store, "t").await;
        let lost = store
            .revoke_module_consent_tracked("m", Some("t"), AT)
            .await
            .unwrap();
        answer(&store, lost, GrantEndStop::Unconfirmed).await;
        let a = grant(&store, "t").await;
        let settling = store
            .revoke_module_consent_tracked("m", Some("t"), AT)
            .await
            .unwrap();
        assert_eq!(
            carried(&store, settling).await.len(),
            2,
            "{a:?} and the lost one's"
        );
        answer(&store, settling, GrantEndStop::Confirmed).await;
    }
    let a = grant(&store, "t").await;
    let next = store
        .revoke_module_consent_tracked("m", Some("t"), AT)
        .await
        .unwrap();
    assert_eq!(carried(&store, next).await, refs(&[&a]));
}

/// The revocation that settled the end the write `generation` wrote.
async fn settled(store: &Store, generation: i64) -> Option<i64> {
    sqlx::query_scalar("SELECT settled_by FROM module_grant_end_notices WHERE generation = ?")
        .bind(generation)
        .fetch_one(store.pool())
        .await
        .unwrap()
}

/// #1202 G7: settled are only the ends a confirmed revocation carried and
/// covers: of its tool, not the module's (`*`), for a tool's; all for the
/// module's; none written after it, nor by an unconfirmed revocation or a
/// confirmed replacement. A settled end is neither pending nor carried.
#[tokio::test]
async fn a_revocation_settles_only_the_ends_it_covers() {
    let store = Store::open_memory().await.unwrap();
    let revoke = |op: Option<&'static str>| {
        let store = store.clone();
        async move {
            store
                .revoke_module_consent_tracked("m", op, AT)
                .await
                .unwrap()
        }
    };
    let pending = || {
        let store = store.clone();
        async move {
            store
                .pending_grant_end_notices("2026-10-26T00:00:00Z")
                .await
                .unwrap()
                .into_iter()
                .map(|end| end.generation)
                .collect::<Vec<_>>()
        }
    };
    let elsewhere = summary("n", "t");
    store
        .grant_module_consent(&elsewhere, AT, UNTIL)
        .await
        .unwrap();
    let other = store
        .revoke_module_consent_tracked("n", Some("t"), AT)
        .await
        .unwrap();
    answer(&store, other, GrantEndStop::Unconfirmed).await;
    let a = grant(&store, "t").await;
    let lost = revoke(Some("t")).await;
    answer(&store, lost, GrantEndStop::Unconfirmed).await;
    let unconfirmed = revoke(Some("t")).await;
    answer(&store, unconfirmed, GrantEndStop::Unconfirmed).await;
    assert_eq!(
        settled(&store, lost).await,
        None,
        "an unconfirmed revocation"
    );
    grant(&store, "t").await;
    let replaced = grant(&store, "t").await.generation;
    answer(&store, replaced, GrantEndStop::Confirmed).await;
    assert_eq!(settled(&store, lost).await, None, "a confirmed replacement");
    let x = grant(&store, "x").await;
    store.advance_module_enable_epoch("m").await.unwrap();
    let epoch = ends(&store).await.pop().unwrap().generation;
    let p = grant(&store, "t").await;
    let told_never = grant(&store, "t").await.generation;
    let tool = revoke(Some("t")).await;
    let d = grant(&store, "t").await;
    let after = grant(&store, "t").await.generation;
    // p replaced the epoch's stale grant: an end of its own, as is the
    // tool's revocation, past its route's grace.
    assert_eq!(
        pending().await,
        vec![epoch, p.generation, told_never, tool, after]
    );
    answer(&store, tool, GrantEndStop::Confirmed).await;
    let by = store.grant_end_notice(tool).await.unwrap().unwrap().id;
    for end in [lost, unconfirmed, p.generation, told_never] {
        assert_eq!(settled(&store, end).await, Some(by));
    }
    for end in [epoch, after, replaced] {
        assert_eq!(
            settled(&store, end).await,
            None,
            "the module's, later, or confirmed"
        );
    }
    assert_eq!(pending().await, vec![epoch, after]);
    let again = carried(&store, revoke(Some("t")).await).await;
    assert!(again.contains(&x.authorization_ref()) && again.contains(&d.authorization_ref()));
    assert!(!again.contains(&a.authorization_ref()) && !again.contains(&p.authorization_ref()));
    // The module's revocation settles every end, the module's too.
    let module = revoke(None).await;
    answer(&store, module, GrantEndStop::Confirmed).await;
    assert!(pending().await.is_empty());
    assert!(carried(&store, revoke(None).await).await.is_empty());
    assert_eq!(settled(&store, other).await, None, "another module's");
}
