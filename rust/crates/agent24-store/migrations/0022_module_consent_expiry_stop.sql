-- K1-6a-G §2.5 (A5b): whether the module said it stopped the work a grant
-- started once the grant expired ('confirmed', 'unconfirmed',
-- 'not_running'). NULL until the kernel told the module: the expiry
-- notifier sends to the granted rows still NULL, and the list reads NULL
-- as unconfirmed. A new decision for the same key clears it.
ALTER TABLE module_consents ADD COLUMN expiry_stop_state TEXT;
