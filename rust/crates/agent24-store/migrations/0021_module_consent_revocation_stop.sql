-- K1-6a-G §2.5 (A5): whether the module said it stopped the work a
-- revocation ended ('confirmed', 'unconfirmed', 'not_running'). NULL until
-- the kernel heard back, or for a revocation no daemon told the module
-- about: read as unconfirmed. A new revocation of the same key clears it.
ALTER TABLE module_consent_revocations ADD COLUMN stop_state TEXT;
