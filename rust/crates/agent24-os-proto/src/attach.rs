//! A3 (`docs/design/A3-ATTACHED-MODULE.md` §5.1–§5.2, review H2) — one
//! attached module's live generation slot.
//!
//! A1's supervisor already owns "one process, one generation at a time" —
//! this is the A3 equivalent for a module the kernel never spawns: at most
//! one LIVE attached [`Generation`] per module, installed when a handshake's
//! registry-locked commit step succeeds (§4.3 ②) and cleared when that
//! generation ends.
//!
//! # `&mut self` is the point (review H2)
//!
//! Every method here takes `&mut self`. That is not an oversight to relax
//! later — it is what makes "check the record is still valid, then install a
//! generation" impossible to split into two steps with a window between them.
//! `AttachSlot` has no interior mutability and no lock of its own; the ONLY
//! way to call [`Self::install`] is to already be holding `&mut` to it, which
//! in `agent24d` means already holding the registry's own lock. So the
//! invariant "at most one live generation per module" is enforced by the type
//! system's aliasing rules, not by a runtime check that something else could
//! race past.

use std::sync::Arc;

use crate::drain::Generation;

/// One attached module's live generation, if it has one right now.
///
/// Lives inside `agent24d`'s registry entry for a module (one `AttachSlot`
/// per registered name), guarded by that registry's own lock — see the
/// module docs for why that is load-bearing rather than incidental.
#[derive(Debug, Default)]
pub struct AttachSlot {
    current: Option<Arc<Generation>>,
    /// The last generation NUMBER handed out for this module, in this
    /// daemon's lifetime (§5.1: "在该模块内单调递增（内存计数）；daemon 重启
    /// 后从 1 重新计"). Not a credential — only for logs and judgement C4's
    /// "generation +1" check.
    last_number: u64,
}

/// [`AttachSlot::install`] found a live generation already installed.
///
/// Corresponds to the wire failure `busy` (§4.3, §5.4 Q3=a: first comer keeps
/// the slot; a second, otherwise-valid handshake is refused rather than
/// replacing it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotBusy;

impl std::fmt::Display for SlotBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("this module already has a live attached generation")
    }
}

impl std::error::Error for SlotBusy {}

impl AttachSlot {
    /// An empty slot: no live generation yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a fresh attached generation, if none is live.
    ///
    /// Builds [`Generation::attached`] and makes it `Running` in the same
    /// step — a slot never holds a generation that is not yet `ready()`,
    /// which keeps [`Self::release`]'s "still holds MY generation" check
    /// meaningful for every generation this type ever hands out. (A fresh
    /// `Generation::attached()` always becomes ready on its first `ready()`
    /// call — only [`Generation::starting`]'s placeholder ever refuses, per
    /// review H1 — so this cannot silently install a generation stuck in
    /// `Starting`.)
    ///
    /// # Errors
    ///
    /// [`SlotBusy`] if a generation is already installed — first comer keeps
    /// it (§5.4 Q3=a); this call otherwise has no side effect.
    pub fn install(&mut self) -> Result<(u64, Arc<Generation>), SlotBusy> {
        if self.current.is_some() {
            return Err(SlotBusy);
        }
        let generation = Generation::attached();
        let became_running = generation.ready();
        debug_assert!(
            became_running,
            "a fresh Generation::attached() must become Running on its first ready()"
        );
        self.last_number += 1;
        self.current = Some(Arc::clone(&generation));
        Ok((self.last_number, generation))
    }

    /// Revoke the live generation, if there is one, and clear the slot.
    ///
    /// Returns whether there was one to revoke. Called from every path that
    /// ends an attached module's run under the registry lock — `DELETE`,
    /// `disable`, a rotating/re-registering `add`, and daemon shutdown's
    /// `revoke_all` (§5.2, §5.3: all of them are "immediate revoke, no
    /// drain"). [`Generation::revoke`] is `pub(crate)`, which is what keeps
    /// generation lifecycle management inside this crate — `agent24d` only
    /// ever calls through here.
    pub fn revoke(&mut self) -> bool {
        match self.current.take() {
            Some(generation) => {
                // One-shot and `#[must_use]`; the `Revocation` itself (which
                // in-flight ids were sent vs never-sent) is not this type's
                // business — attach connections have no proxied requests to
                // report on (§5.3).
                let _ = generation.revoke();
                true
            }
            None => false,
        }
    }

    /// Called by the connection task when ITS OWN generation ended (EOF, a
    /// write failure, a `stop` firing — see `attach_mux.rs`). Clears the slot
    /// ONLY if it still holds that exact generation: if a revoke-then-install
    /// (a rotation, or a fresh connection after this one already lost the
    /// race — though §5.4 Q3=a means a second live connection cannot exist
    /// concurrently, a stale `release` from an already-superseded generation
    /// can still arrive after) replaced it with a NEWER one in the meantime,
    /// that newer generation is left alone. Comparing by identity
    /// (`Arc::ptr_eq`), not by generation number, because the number is not
    /// available to a caller that only has the `Arc` — and identity is the
    /// stronger, more direct statement of "is this the same run".
    pub fn release(&mut self, generation: &Arc<Generation>) {
        if self
            .current
            .as_ref()
            .is_some_and(|live| Arc::ptr_eq(live, generation))
        {
            self.current = None;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn install_on_an_empty_slot_yields_a_running_attached_generation() {
        let mut slot = AttachSlot::new();
        let (number, generation) = slot.install().expect("an empty slot installs");
        assert_eq!(number, 1);
        assert_eq!(
            generation.upstream(),
            None,
            "an attached module has no proxy socket"
        );
        // `ready()` already returned `true` inside `install`, so a second
        // call must report "already ready" (`false`) — proving the slot did
        // not leave the generation in `Starting`.
        assert!(!generation.ready());
    }

    /// §5.4 Q3=a: first comer keeps the slot.
    #[test]
    fn a_second_install_while_one_is_live_is_refused() {
        let mut slot = AttachSlot::new();
        let (_, first) = slot.install().expect("first install");
        assert_eq!(slot.install().unwrap_err(), SlotBusy);
        // Control: the refusal had no side effect — the first generation is
        // still exactly the one installed, untouched.
        assert!(Arc::ptr_eq(&first, &slot.current.clone().unwrap()));
    }

    /// Generation numbers are monotonic within the slot's lifetime, not reset
    /// by a revoke+reinstall (C4's "第一条断开后重连成功且 generation +1").
    #[test]
    fn generation_numbers_increase_across_a_revoke_and_reinstall() {
        let mut slot = AttachSlot::new();
        let (first_number, _) = slot.install().unwrap();
        assert!(slot.revoke());
        let (second_number, _) = slot.install().unwrap();
        assert_eq!(second_number, first_number + 1);
    }

    #[test]
    fn revoke_on_an_empty_slot_reports_nothing_to_revoke() {
        let mut slot = AttachSlot::new();
        assert!(!slot.revoke());
    }

    #[test]
    fn revoke_actually_revokes_the_generation_not_just_the_slot() {
        let mut slot = AttachSlot::new();
        let (_, generation) = slot.install().unwrap();
        assert!(slot.revoke());
        assert_eq!(generation.state(), crate::drain::DrainState::Revoked);
        // A second install is possible immediately — nothing is left "busy".
        assert!(slot.install().is_ok());
    }

    /// The property review H2 calls out by name: `release` clears the slot
    /// ONLY if it still holds the caller's exact generation. A stale
    /// `release` from an already-superseded generation must leave a NEWER
    /// one installed afterward untouched.
    #[test]
    fn release_of_a_superseded_generation_does_not_clear_the_newer_one() {
        let mut slot = AttachSlot::new();
        let (_, stale) = slot.install().unwrap();
        assert!(slot.revoke());
        let (_, current) = slot.install().unwrap();

        slot.release(&stale);
        assert!(
            slot.current
                .as_ref()
                .is_some_and(|live| Arc::ptr_eq(live, &current)),
            "a stale release must not evict the current generation"
        );

        slot.release(&current);
        assert!(
            slot.current.is_none(),
            "the current generation's own release DOES clear it"
        );
    }
}
