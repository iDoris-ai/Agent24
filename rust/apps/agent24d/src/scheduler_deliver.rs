//! ME4-1.3.1 — design `docs/design/ME4-S1-scheduler-callback.md` §5.1/§5.3:
//! the module deliverer. Turns one module fire
//! (`agent24_scheduler::ScheduleInvocation` with `InvocationTarget::Module`)
//! into a real `POST /api/v1/<ns>/_a24/scheduler/fired` over the module's
//! live `Generation` (`agent24_os_proto::kernel_call::send_kernel_request`),
//! and classifies the transport result into the `FireOutcome` the scheduler's
//! delivery state machine (`agent24_scheduler::deliveries::apply_outcome`)
//! consumes.
//!
//! `KernelTrigger` (`server.rs`)'s `Module` arm delegates here for every fire
//! the delivery pump (`agent24_scheduler::deliveries::DeliveryPump`) drives —
//! tick itself never reaches this (design §3.2).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use agent24_os_proto::drain::{Abandoned, RequestRefused};
use agent24_os_proto::kernel_call::{
    FIRE_ID_HEADER, KernelCallError, KernelLimits, KernelRequest, KernelRequestIds, KernelResponse,
    SCHEDULE_KEY_HEADER, send_kernel_request,
};
use agent24_scheduler::{DeferReason, FireId, FireOutcome, ModuleScheduleKey};
use axum::http::{HeaderName, HeaderValue};

use crate::domain::Supervisors;

/// design §5.4/v3 L-F: production limits — 10s per attempt, 64 KiB of
/// response body read (and discarded). Mirrors
/// `agent24_scheduler::deliveries::{DELIVERY_TIMEOUT, MAX_FIRED_RESPONSE_BYTES}`
/// exactly, so the two crates cannot silently drift apart.
pub const PRODUCTION_LIMITS: KernelLimits = KernelLimits {
    total: Duration::from_secs(agent24_scheduler::deliveries::DELIVERY_TIMEOUT.as_secs()),
    max_response_bytes: agent24_scheduler::deliveries::MAX_FIRED_RESPONSE_BYTES,
};

/// design §5.1's late-bound handle to the running supervisors, plus the
/// kernel request-id minter and the injected per-attempt limits.
/// `AppState::new` builds the `Scheduler` (and its `KernelTrigger`) before
/// `mount_all` has a `ProcessHost` to hand this an `Arc<Supervisors>` —
/// `server::serve` calls [`Self::set_supervisors`] right after `mount_all`
/// returns, before the tick loop and the delivery pump start (design §4.6).
/// Unset ⇒ every module fire is `Deferred(MountPending)`.
pub struct ModuleDeliverer {
    supervisors: OnceLock<Arc<Supervisors>>,
    ids: KernelRequestIds,
    limits: KernelLimits,
}

impl ModuleDeliverer {
    #[must_use]
    pub fn new(limits: KernelLimits) -> Self {
        Self {
            supervisors: OnceLock::new(),
            ids: KernelRequestIds::new_random(),
            limits,
        }
    }

    /// Set exactly once, right after `mount_all` returns and before the tick
    /// loop / delivery pump start (design §4.6). A second call is a
    /// programming error in the caller — `OnceLock::set` silently keeps the
    /// first value either way, so this only logs loudly rather than panics: a
    /// background deliverer is not the place to bring the daemon down over a
    /// startup-ordering bug that did not actually lose any data.
    pub fn set_supervisors(&self, supervisors: Arc<Supervisors>) {
        if self.supervisors.set(supervisors).is_err() {
            tracing::warn!(
                "ModuleDeliverer::set_supervisors called more than once; the first call wins — \
                 this should happen exactly once, right after mount_all"
            );
            debug_assert!(
                false,
                "ModuleDeliverer::set_supervisors called twice — should be set exactly once, \
                 after mount_all"
            );
        }
    }

    /// design §5.1/§5.3: find the module's live generation, then delegate to
    /// [`Self::deliver_on`]. Never panics, never blocks the tick — the
    /// delivery pump awaits this once per attempt, at most
    /// `PER_OWNER_IN_FLIGHT` at a time per owner.
    pub async fn deliver(
        &self,
        owner: &ModuleScheduleKey,
        fire_id: &FireId,
        trigger: &str,
        scheduled_for: &str,
        fired_at: &str,
    ) -> FireOutcome {
        let Some(supervisors) = self.supervisors.get() else {
            return FireOutcome::Deferred {
                reason: DeferReason::MountPending,
            };
        };
        let Some(current) = supervisors.running_slot(&owner.owner_module) else {
            return FireOutcome::Deferred {
                reason: DeferReason::NotRunning,
            };
        };
        self.deliver_on(
            &current.get(),
            owner,
            fire_id,
            trigger,
            scheduled_for,
            fired_at,
        )
        .await
    }

    /// Review round 1, **M3**: the actual send, split out of
    /// [`Self::deliver`] so a test can drive it against a REAL
    /// `Generation`/UDS mock upstream directly — without needing a real
    /// `Supervisors`/`SupervisorHandle` (which, per this module's own doc
    /// comment, has no public constructor outside a genuinely supervised
    /// process). Builds the fired request from `owner`/`fire_id`/`trigger`/
    /// `scheduled_for`/`fired_at`, sends it, and classifies the result.
    async fn deliver_on(
        &self,
        generation: &Arc<agent24_os_proto::drain::Generation>,
        owner: &ModuleScheduleKey,
        fire_id: &FireId,
        trigger: &str,
        scheduled_for: &str,
        fired_at: &str,
    ) -> FireOutcome {
        let namespace = agent24_domain::DomainOsManifest::declared_namespace(&owner.owner_module);
        let path = format!("{namespace}/_a24/scheduler/fired");
        let body = FiredBody {
            key: &owner.module_key,
            trigger,
            scheduled_for,
            fired_at,
        };
        let body_bytes = match serde_json::to_vec(&body) {
            Ok(b) => b,
            Err(err) => {
                // Unreachable in practice — every field is a plain string —
                // fail safe rather than panic a background loop on a
                // serialization bug.
                return FireOutcome::Failed {
                    reason: format!("kernel bug: could not serialize the fired body: {err}"),
                };
            }
        };
        let (Ok(key_header), Ok(fire_id_header)) = (
            HeaderValue::from_str(&owner.module_key),
            HeaderValue::from_str(fire_id.as_str()),
        ) else {
            // `module_key` is validated at upsert time (design §6.3: ASCII
            // `[a-z0-9._-]`) and `fire_id` is kernel-derived hex — neither
            // should ever fail to become a header value; fail safe rather
            // than panic if that invariant is ever violated.
            return FireOutcome::Failed {
                reason: "kernel bug: schedule key or fire id is not a valid header value"
                    .to_owned(),
            };
        };
        let request = KernelRequest {
            path,
            extra_headers: vec![
                (HeaderName::from_static(SCHEDULE_KEY_HEADER), key_header),
                (HeaderName::from_static(FIRE_ID_HEADER), fire_id_header),
            ],
            body: body_bytes.into(),
        };
        let result = send_kernel_request(generation, &self.ids, request, self.limits, None).await;
        classify(fire_id, result)
    }
}

/// design §5.3: the body of `POST /api/v1/<ns>/_a24/scheduler/fired`. Stable
/// across retries — every field is read back off the delivery row by the
/// caller (`agent24_scheduler::deliveries::run_attempt`), never re-taken at
/// send time.
#[derive(Debug, serde::Serialize)]
struct FiredBody<'a> {
    key: &'a str,
    /// `"tick"` | `"run_now"`.
    trigger: &'a str,
    scheduled_for: &'a str,
    fired_at: &'a str,
}

/// design §5.3's classification table: one attempt's transport result → the
/// [`FireOutcome`] the delivery state machine consumes. The rule: **not sent
/// ⇒ `Deferred`; sent (or a live generation refused the connection) and not
/// 2xx ⇒ `Failed`**.
#[must_use]
fn classify(fire_id: &FireId, result: Result<KernelResponse, KernelCallError>) -> FireOutcome {
    let failed = |reason: String| FireOutcome::Failed { reason };
    let deferred = |reason| FireOutcome::Deferred { reason };
    match result {
        Ok(r) if r.status.is_success() => FireOutcome::ModuleDelivered {
            fire_id: fire_id.clone(),
        },
        Ok(r) => failed(format!("module answered HTTP {}", r.status.as_u16())),
        Err(KernelCallError::Refused(RequestRefused::NotReady)) => deferred(DeferReason::NotReady),
        Err(KernelCallError::Refused(RequestRefused::Draining)) => deferred(DeferReason::Draining),
        Err(KernelCallError::Refused(RequestRefused::Stopping)) => deferred(DeferReason::Stopping),
        Err(
            KernelCallError::Refused(RequestRefused::DuplicateId)
            | KernelCallError::EntropyUnavailable,
        ) => deferred(DeferReason::KernelTransient),
        Err(
            KernelCallError::NotDispatched
            | KernelCallError::Abandoned(Abandoned { dispatched: false }),
        ) => deferred(DeferReason::NeverSent),
        // Sent, then the generation was revoked under it: the module may have
        // acted (at-least-once ⇒ redeliver), and a handler that crashes the
        // module every time must not loop forever — so this is a SENT
        // attempt, not a deferral (design §5.3, residual risk R11).
        Err(KernelCallError::Abandoned(Abandoned { dispatched: true })) => {
            failed("the module was stopped while the delivery was in flight".to_owned())
        }
        Err(KernelCallError::NotSent(e)) => failed(format!("could not reach the module: {e}")),
        Err(KernelCallError::MaybeSent(e)) => {
            failed(format!("connection closed during the delivery: {e}"))
        }
        Err(KernelCallError::Timeout) => {
            failed("the module did not answer within the delivery timeout".to_owned())
        }
        Err(KernelCallError::ResponseTooLarge) => {
            failed("the module's response exceeded the limit".to_owned())
        }
        // Review round 1, L3: a non-2xx response whose body failed to read
        // for a reason OTHER than the size limit (a reset/truncated
        // connection, most likely) — still a real failure, `last_error`
        // says what actually happened rather than the size-limit message.
        Err(KernelCallError::ResponseBodyError(e)) => {
            failed(format!("the module's response body could not be read: {e}"))
        }
        // A `Running` generation always has an upstream address (`Generation::
        // ready`'s own invariant) — unreachable in practice; treated as a
        // transient kernel condition rather than trusted to be impossible.
        Err(KernelCallError::NoUpstream) => deferred(DeferReason::NotReady),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use agent24_os_proto::drain::Generation;
    use agent24_scheduler::FireTrigger;

    fn fid() -> FireId {
        FireId::derive(FireTrigger::Tick, "sch_test", chrono::Utc::now())
    }

    /// Review round 1, **M3**, judgement **C4.1** (end to end this time, not
    /// just at the `classify` unit level): `deliver_on` against a REAL
    /// `Generation` and a real UDS mock upstream — proves the path is built
    /// from `DomainOsManifest::declared_namespace` (never hand-assembled a
    /// second way) and the body is exactly the `FiredBody` shape design §5.3
    /// promises: `{key, trigger, scheduled_for, fired_at}`.
    ///
    /// Mutation: rename a `FiredBody` field (e.g. `key` → `module_key`), or
    /// hand-build the path without `declared_namespace` — this test goes
    /// red (the JSON keys / path this test asserts on stop matching).
    #[tokio::test]
    async fn deliver_on_posts_the_declared_namespace_path_and_the_fired_body_shape() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        fn unique_sock(tag: &str) -> std::path::PathBuf {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            std::path::PathBuf::from(format!(
                "/tmp/a24-scheduler-deliver-{tag}-{}-{nanos}.sock",
                std::process::id()
            ))
        }

        let path = unique_sock("c4-1");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                let Some(idx) = text.find("\r\n\r\n") else {
                    continue;
                };
                let head = text[..idx].to_owned();
                let content_length: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().to_owned())
                    })
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                if buf.len() < idx + 4 + content_length {
                    continue;
                }
                let body = buf[idx + 4..idx + 4 + content_length].to_vec();
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await;
                let _ = tx.send((head, body));
                return;
            }
        });
        let generation = Generation::serving_at(path);
        assert!(generation.ready());
        let deliverer = ModuleDeliverer::new(PRODUCTION_LIMITS);
        let owner = ModuleScheduleKey {
            owner_module: "zzmod".to_owned(),
            module_key: "k1".to_owned(),
        };
        // Captured ONCE: `fid()` derives from `Utc::now()`, so calling it
        // again for the assertion below could (rarely, across a second
        // boundary) mint a DIFFERENT id than the one actually sent.
        let fire_id = fid();
        let outcome = deliverer
            .deliver_on(
                &generation,
                &owner,
                &fire_id,
                "tick",
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:05Z",
            )
            .await;
        assert!(
            matches!(outcome, FireOutcome::ModuleDelivered { .. }),
            "{outcome:?}"
        );

        let (head, body) = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("the mock upstream never received a complete request")
            .unwrap();
        let expected_path = format!(
            "{}/_a24/scheduler/fired",
            agent24_domain::DomainOsManifest::declared_namespace("zzmod")
        );
        assert!(
            head.starts_with(&format!("POST {expected_path} HTTP/1.1")),
            "{head}"
        );
        // Review round 2, M-C: the header contract itself, not just the path
        // and the body shape. `x-a24-schedule-key`/`x-a24-fire-id` must
        // carry the EXACT values this call was given;
        // `x-a24-request-id`/`x-a24-approval-token` must simply be present
        // (their values are internal, minted per attempt — design §5.2).
        let lower = head.to_ascii_lowercase();
        assert!(
            lower
                .lines()
                .any(|l| l.trim() == format!("x-a24-fire-id: {}", fire_id.as_str())),
            "{head}"
        );
        assert!(
            lower.lines().any(|l| l.trim() == "x-a24-schedule-key: k1"),
            "{head}"
        );
        assert!(
            lower
                .lines()
                .any(|l| l.trim_start().starts_with("x-a24-request-id:")),
            "{head}"
        );
        assert!(
            lower
                .lines()
                .any(|l| l.trim_start().starts_with("x-a24-approval-token:")),
            "{head}"
        );
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["key"], "k1");
        assert_eq!(json["trigger"], "tick");
        assert_eq!(json["scheduled_for"], "2026-01-01T00:00:00Z");
        assert_eq!(json["fired_at"], "2026-01-01T00:00:05Z");
    }

    #[test]
    fn a_2xx_status_is_module_delivered() {
        let outcome = classify(
            &fid(),
            Ok(KernelResponse {
                status: axum::http::StatusCode::OK,
            }),
        );
        assert_eq!(outcome, FireOutcome::ModuleDelivered { fire_id: fid() });
    }

    #[test]
    fn a_non_2xx_status_is_failed_not_deferred() {
        let outcome = classify(
            &fid(),
            Ok(KernelResponse {
                status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            }),
        );
        match outcome {
            FireOutcome::Failed { reason } => assert!(reason.contains("500")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn not_ready_draining_stopping_are_all_deferred_not_failed() {
        for (refused, expect) in [
            (RequestRefused::NotReady, DeferReason::NotReady),
            (RequestRefused::Draining, DeferReason::Draining),
            (RequestRefused::Stopping, DeferReason::Stopping),
        ] {
            let outcome = classify(&fid(), Err(KernelCallError::Refused(refused)));
            assert_eq!(outcome, FireOutcome::Deferred { reason: expect });
        }
    }

    #[test]
    fn never_sent_is_deferred_not_failed() {
        let outcome = classify(&fid(), Err(KernelCallError::NotDispatched));
        assert_eq!(
            outcome,
            FireOutcome::Deferred {
                reason: DeferReason::NeverSent
            }
        );
        let outcome = classify(
            &fid(),
            Err(KernelCallError::Abandoned(Abandoned { dispatched: false })),
        );
        assert_eq!(
            outcome,
            FireOutcome::Deferred {
                reason: DeferReason::NeverSent
            }
        );
    }

    /// design §5.3: `Abandoned{dispatched:true}` is a FAILED attempt, never a
    /// deferral — the whole point being that a module which crashes on every
    /// fired delivery must eventually hit `failed`, not loop forever.
    #[test]
    fn dispatched_and_abandoned_is_failed_not_deferred() {
        let outcome = classify(
            &fid(),
            Err(KernelCallError::Abandoned(Abandoned { dispatched: true })),
        );
        assert!(matches!(outcome, FireOutcome::Failed { .. }), "{outcome:?}");
    }

    #[test]
    fn timeout_and_response_too_large_are_failed() {
        assert!(matches!(
            classify(&fid(), Err(KernelCallError::Timeout)),
            FireOutcome::Failed { .. }
        ));
        assert!(matches!(
            classify(&fid(), Err(KernelCallError::ResponseTooLarge)),
            FireOutcome::Failed { .. }
        ));
    }

    /// Review round 1, L3: a non-2xx response body that fails to read for a
    /// reason OTHER than the size limit is still `Failed` (sent, just not
    /// acknowledged) — never silently reclassified as `ResponseTooLarge`.
    #[test]
    fn a_response_body_error_is_failed_and_says_what_happened() {
        let outcome = classify(
            &fid(),
            Err(KernelCallError::ResponseBodyError(
                "connection reset".to_owned(),
            )),
        );
        match outcome {
            FireOutcome::Failed { reason } => assert!(reason.contains("connection reset")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    fn owner() -> ModuleScheduleKey {
        ModuleScheduleKey {
            owner_module: "zzmod".to_owned(),
            module_key: "k".to_owned(),
        }
    }

    /// design §4.6/§5.1: before `set_supervisors` is ever called, every
    /// module fire is `Deferred(MountPending)` — never a failure.
    #[tokio::test]
    async fn unset_supervisors_defers_as_mount_pending() {
        let deliverer = ModuleDeliverer::new(PRODUCTION_LIMITS);
        let outcome = deliverer
            .deliver(
                &owner(),
                &fid(),
                "tick",
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
            )
            .await;
        assert_eq!(
            outcome,
            FireOutcome::Deferred {
                reason: DeferReason::MountPending
            }
        );
    }

    /// design §5.1/§9: an owner with no running slot (never mounted,
    /// disabled, hot-disabled, uninstalled) is `Deferred(NotRunning)`.
    #[tokio::test]
    async fn no_running_slot_defers_as_not_running() {
        let deliverer = ModuleDeliverer::new(PRODUCTION_LIMITS);
        deliverer.set_supervisors(Arc::new(Supervisors::default()));
        let outcome = deliverer
            .deliver(
                &owner(),
                &fid(),
                "tick",
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
            )
            .await;
        assert_eq!(
            outcome,
            FireOutcome::Deferred {
                reason: DeferReason::NotRunning
            }
        );
    }

    /// Review round 2, **L-e**: a `Starting` generation (handshake not
    /// complete) is `Deferred(NotReady)`, never a failure — same admission
    /// every proxied request goes through. Now exercised directly through
    /// `deliver_on` (the M3 split, round 1): the stale comment this test
    /// used to carry claimed this was "proven end-to-end ... in the
    /// black-box test module instead" — untrue (ME4-1.5.1's black-box suite
    /// does not exist yet in this cut) — and settled for pinning
    /// `admit_request`'s own refusal one layer below `deliver_on` instead of
    /// this module's own code path.
    #[tokio::test]
    async fn a_starting_generation_defers_as_not_ready() {
        let generation = Generation::starting();
        let deliverer = ModuleDeliverer::new(PRODUCTION_LIMITS);
        let owner = ModuleScheduleKey {
            owner_module: "zzmod".to_owned(),
            module_key: "k1".to_owned(),
        };
        let outcome = deliverer
            .deliver_on(
                &generation,
                &owner,
                &fid(),
                "tick",
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
            )
            .await;
        assert_eq!(
            outcome,
            FireOutcome::Deferred {
                reason: DeferReason::NotReady
            }
        );
    }

    /// design §11 C4.11: the tick loop AND the delivery pump must be spawned
    /// AFTER `mount_all` returns (design §4.6/§3.2, S1-6) — a structural
    /// scan of `server.rs`'s own source, the same heuristic-but-forcing style
    /// `domain.rs`'s `reserved_segments_match_the_kernel_routes_exactly`
    /// already uses. Mutation: swap the two blocks in `server.rs` — this
    /// assertion goes red.
    #[test]
    fn the_tick_loop_and_delivery_pump_are_spawned_after_mount_all() {
        let src = include_str!("server.rs");
        let mount_at = src
            .find("crate::domain::mount_all(")
            .expect("the mount_all call must exist in server.rs");
        let tick_at = src
            .find("tick_scheduler.run(")
            .expect("the tick loop spawn must exist in server.rs");
        let pump_at = src
            .find("DeliveryPump::new(")
            .expect("the delivery pump spawn must exist in server.rs");
        assert!(
            tick_at > mount_at,
            "the scheduler tick loop must be spawned after mount_all (design §4.6)"
        );
        assert!(
            pump_at > mount_at,
            "the delivery pump must be spawned after mount_all (design §4.6)"
        );

        // Review round 1, L5: also pin exactly how many `.run(` spawns exist
        // inside `serve` itself — the tick loop's and the pump's, and no
        // more, no fewer (a regression that spawns the pump twice, or drops
        // one of the two spawns while leaving a stray `.run(`-shaped call
        // sitting around, would slip past the two `find`-based checks above,
        // which only look for the FIRST occurrence of each marker).
        let serve_start = src
            .find("pub async fn serve(")
            .expect("serve must exist in server.rs");
        let serve_body = &src[serve_start..];
        let serve_end = serve_body
            .find("\n}\n")
            .expect("serve must be brace-terminated");
        let serve_body = &serve_body[..serve_end];
        // Review round 2, L-d: strip `//` line comments first — otherwise an
        // EXPLANATORY comment mentioning a third `.run(`-shaped call (a
        // perfectly normal thing to write while explaining why something is
        // NOT spawned, say) would inflate this count and make the test
        // falsely red for a change that never touched real code.
        let serve_body_no_comments: String = serve_body
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let run_spawns = serve_body_no_comments.matches(".run(").count();
        assert_eq!(
            run_spawns, 2,
            "serve() must spawn exactly two `.run(` loops (the tick scheduler and the delivery \
             pump) — found {run_spawns}"
        );
    }
}
