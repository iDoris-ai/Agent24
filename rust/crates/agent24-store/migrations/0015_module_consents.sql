-- K1-6a.1 (ADR-K1-03 §1, "K1-6a 实现切片建议" item 1): kernel-private,
-- per-tool enablement consent records. NOT the existing per-call
-- `module_approvals`/`approvals` approval pipeline — this is the host's
-- durable record of a one-time, per-tool enablement confirmation that lets
-- later in-scope calls skip repeated prompting (6a.2, not implemented yet).
--
-- Tool identity is the (module, op) string pair this slice uses in place of
-- K1-5.1's not-yet-merged tool registry types (per task spec; reconcile once
-- K1-5.1 lands).
--
-- (module, op) is the PRIMARY KEY — no surrogate id, unlike
-- `module_approvals`: there is at most one *current* consent decision per
-- tool. A new grant/deny for the same (module, op) replaces it, which is
-- exactly the "重新同意" the ADR requires whenever a module's permission
-- range or version changes (§4) — the caller always writes a fresh row
-- rather than mutating fields of an old one in place.
--
-- `module_version`/`scope_fingerprint` are captured at decision time so a
-- later lookup can detect drift (ADR §4: "权限、模块版本...变化时，旧许可
-- 不能被自动扩张") by comparing against the CURRENT module's version/
-- fingerprint, without this table knowing anything about what produced
-- them.
CREATE TABLE module_consents (
    module             TEXT NOT NULL,
    op                 TEXT NOT NULL,
    module_version     TEXT NOT NULL,
    scope_fingerprint  TEXT NOT NULL,
    source             TEXT NOT NULL CHECK (source IN ('first_party', 'manual_install')),
    -- Host-determined risk (ADR §1 "风险" row) — already clamped by the
    -- caller (`ToolPermissionSummary::new`) so a `manual_install` row can
    -- never be stored below `high`; this table does not re-derive or
    -- re-check that floor itself.
    risk               TEXT NOT NULL CHECK (risk IN ('low', 'medium', 'high')),
    readable           TEXT,  -- NULL = no read permission (ADR §1: absence
    writable           TEXT,  -- must be shown as explicit "无", never
    external           TEXT,  -- omitted — these columns' nullability IS that).
    decision           TEXT NOT NULL CHECK (decision IN ('granted', 'denied')),
    decided_at         TEXT NOT NULL,
    -- ADR §3: every licence must carry a finite, checkable expiry — no
    -- "forever" grant. This migration only enforces NOT NULL; picking a
    -- bounded value is the issuing caller's responsibility (host UI / 6a.2).
    expires_at         TEXT NOT NULL,
    PRIMARY KEY (module, op)
);
