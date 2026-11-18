-- Kernel-capability consent for an out-of-process OS package.
-- Separate from module_consents: that table is per tool and follows
-- A24_MODULE_TOOL_GRANTS. This row is the consent card (CONSENT-2).
CREATE TABLE kernel_consents (
    module TEXT PRIMARY KEY NOT NULL,
    fingerprint TEXT NOT NULL,
    capabilities TEXT NOT NULL,
    team_id TEXT,
    version TEXT NOT NULL,
    decided_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
