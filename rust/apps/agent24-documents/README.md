# agent24-documents

Documenting domain OS: the first-party, out-of-process module `documents` ([ADR-DOC-01](../../../docs/documenting/adr/ADR-DOC-01-placement-and-integration.md)). The kernel serves it under `/api/v1/documents`, and it keeps its data in `~/.agent24/os/documents/`.

## Development install

There is no first-party default installation yet (ADR-DOC-01 D6), so for development, install it by hand.

1. Build the module into the shared target directory (AGENTS.md: worktrees share the main checkout's target):

   ```sh
   export CARGO_TARGET_DIR=<main checkout>/rust/target
   cd rust && cargo build -p agent24-documents
   ```

2. Assemble a package directory. The manifest's `spawn.command` is `bin/agent24-documents`, relative to that directory. On macOS, also build the read engine into the same `bin/` ([read engine](../../../docs/documenting/engine-pdfkit.md)):

   ```sh
   mkdir -p /tmp/documents-pkg/bin
   cp apps/agent24-documents/domain-os.yml /tmp/documents-pkg/
   cp "$CARGO_TARGET_DIR/debug/agent24-documents" /tmp/documents-pkg/bin/
   apps/agent24-documents/engines/pdfkit/build.sh /tmp/documents-pkg/bin   # macOS only
   ```

3. Install the package, then (re)start the resident daemon:

   ```sh
   agent24 os install /tmp/documents-pkg
   ```

4. Check it:

   ```sh
   curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:$PORT/api/v1/documents/capabilities
   ```

Platform: macOS first. On Linux the OS runs, but the slice-1 engines are macOS-only, so engine-backed operations report `engine_unavailable` (ADR-DOC-01 D6/D10, 2026-10-08 amendments). Windows waits for DEP-C2.

