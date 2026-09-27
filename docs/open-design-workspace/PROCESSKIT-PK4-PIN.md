# ProcessKit PK4 dependency pin

The Windows-only `agent24-sidecar-host` dependency is pinned to the PK4 fork revision `60aa827db378daa5b1ec638f3b3fdaaf9c201560` at `https://github.com/jhfnetboy/ProcessKit-rs.git`. PK4 descends directly from the audited PK3 exact head `0a03685c25fe4d5a962fcc1e6d53f025bb36be51`; both retain package version `3.3.4` and the upstream `v3.3.4` release ancestry at `ba1a6fe77cedad7e1ceeb9f30b158c94b5dc1bb6`. Cargo resolves the immutable full Git SHA; default features remain enabled and `stats` is requested.

This is a dependency pin only. The owner/API adoption gate in [PROCESSKIT-ISOLATED-PIPED-SPAWN.md](PROCESSKIT-ISOLATED-PIPED-SPAWN.md) remains in force. Review and local checks establish the resolved source and build metadata; native Windows runtime behavior and the handle/EOF proofs still require Windows CI.
