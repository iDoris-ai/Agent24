# COMM-6a desktop communication overview

This slice adds a read-only communication overview and explicit daemon/unlock controls. The page uses the desktop IPC backend proxy through typed communication API wrappers. It does not access Hyphae files, spawn a process, call `fetch`, or invoke a run, model, or module.

## Included

- Read-only identities (including the single `default:true` identity and encryption state), contacts and public keys, and relay URLs/source/configured state.
- Independent display of daemon `process`, latest manual `relay_probe`, and `catch_up` state. A running process or successful WebSocket handshake says nothing about catch-up completion or peer delivery. The UI never claims catch-up completed or a message was delivered.
- Daemon start and stop only from explicit button clicks. A pending write disables all write controls to prevent duplicate submissions.
- Unlock via a password input and an unchecked-by-default remember option. The input is cleared immediately on submission and again on success or failure. The page does not render, log, or persist the submitted password.
- Refresh failures retain the last successfully loaded value for each section and identify it as potentially stale. Request sequence tokens reject older refresh/action results and unmount results.
- Known daemon reasons such as `binary_rejected`, `not_configured`, and `locked` remain visible as safe reason labels.

## Explicitly deferred

This is not full COMM-6. Import is deferred until COMM-6b / the import gate has passed. Identity creation/default changes, contact editing, relay editing/probing, send, history, outbox, and retries are also outside this slice. COMM-7 and the two-repository integration gate remain unpassed.

## Acceptance checks

- [ ] Positive and negative process, relay, and catch-up states are independent; no green healthy presentation for `locked`, `gave_up`, or unconfigured relays.
- [ ] A failed refresh preserves already displayed data and marks it potentially stale.
- [ ] A late response cannot overwrite a newer refresh/action result or state after unmount.
- [ ] Unlock clears the password after both success and failure; remember defaults to false; no password appears in rendered output or persistent storage.
- [ ] Pending writes reject duplicate clicks.
- [ ] Network access is exclusively through the desktop IPC proxy wrapper; the page does not call `fetch`, spawn processes, or access the communication HOME.
- [ ] Targeted component tests, renderer typecheck, diff check, and `pre-pr-check.sh` pass for the selected API base.
