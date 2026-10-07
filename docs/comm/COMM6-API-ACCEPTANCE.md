# COMM-6a typed renderer API acceptance

COMM-6a adds the typed renderer-side client for the existing COMM endpoints. It is a slice of COMM-6, not completion of the full UI/API workflow: identity/contact/relay reads, daemon status/start/stop, and unlock are included; identity/contact/relay mutations, import, and message sending remain out of scope.

## Contract and transport

All calls use `window.agent24.backendProxy`; the renderer does not call `fetch`, read files, or spawn processes. The Electron preload forwards the method, path, and optional JSON body over `backend:proxy`. The main-process proxy accepts the COMM `/api/v1/comm/*` paths for GET and POST, attaches its configured bearer token, and returns `{ ok, status, data }` with the parsed HTTP JSON body unchanged. The COMM router body is itself an envelope, `{ ok: true, data }`, which the client validates and unwraps.

| Export | Route | Result |
| --- | --- | --- |
| `listCommIdentities()` | `GET /api/v1/comm/identity` | `CommIdentity[]` |
| `listCommContacts()` | `GET /api/v1/comm/contact` | `CommContact[]` |
| `listCommRelays()` | `GET /api/v1/comm/relay` | `CommRelayConfig` |
| `getCommStatus()` | `GET /api/v1/comm/daemon` | `CommDaemonStatus` |
| `startCommDaemon()` | `POST /api/v1/comm/daemon/start` | `CommDaemonStatus` |
| `stopCommDaemon()` | `POST /api/v1/comm/daemon/stop` | `CommDaemonStatus` |
| `unlockComm(password, remember = false)` | `POST /api/v1/comm/unlock` | `CommUnlockResult` |

The exported interfaces mirror the current Rust router response schemas. The daemon status preserves process state/reason, relay configuration and probe fields, and catch-up state. `CommApiError` carries HTTP status and a backend error code when available so UI can distinguish conditions such as `locked`, `not_configured`, and `binary_rejected`.

## Failure and secret handling

HTTP/proxy failures, an API `{ ok: false }` envelope, malformed envelopes, and malformed required fields reject with `CommApiError`; the client never substitutes an empty list or default status. Requests are made once without automatic retry. Unlock sends the password only in the IPC request body, does not log or persist it, and sanitizes errors that contain the submitted password.

## Verification

From `apps/desktop`, run the targeted unit suite and renderer typecheck:

```sh
pnpm exec vitest run --config vitest.config.ts src/renderer/pages/comm/api.test.ts
pnpm exec tsc --noEmit -p tsconfig.renderer.json
```

The tests cover nested envelopes, all read/status routes, start/stop and unlock bodies, HTTP 401 and API errors, malformed responses, IPC rejection/no retry, password non-disclosure, and the `remember` option. These are renderer-client contract tests; they do not replace end-to-end Electron/daemon integration testing.
