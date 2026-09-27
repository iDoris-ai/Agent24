# ProcessKit isolated piped spawn dependency

Status: hard prerequisite for activating the sidecar host production `run()` on Windows.

## Boundary

Agent24 owns the host-session protocol and lifecycle policy. ProcessKit owns native process creation and Windows Job containment. Neither layer may invent workspace, run-allocation, ACP, or desktop module fields.

The dormant `HostStdio` seam safely duplicates the host's stdin and stdout for its workers. Those duplicates are independently owned and non-inheritable, but they do not change the inheritance flags on the original Windows standard handles.

ProcessKit 3.3.4 ultimately uses the standard Windows process spawn path with handle inheritance enabled. Configuring the target's stdin, stdout, and stderr selects its standard handles, but does not exclude other inheritable handles from the child handle table. Production activation therefore requires an explicit child-handle allowlist; duplicate-only host stdio is not sufficient.

## Required safe API

The concrete name may change, but ProcessKit must expose a safe operation equivalent to:

```text
ProcessGroup::spawn_isolated_piped(command) -> IsolatedPipedChild
```

The result must retain authoritative process-tree ownership and expose owned parent-side stdin, stdout, and stderr endpoints plus the existing bounded observe, stop, and reap operations. Agent24 must not reconstruct ownership from raw handles.

On Windows the implementation must:

1. create three pipe pairs and make only the child ends inheritable;
2. place exactly those child ends in `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`;
3. set the three `STARTF_USESTDHANDLES` fields to those child ends;
4. call `CreateProcessW` with extended startup information and suspended creation;
5. assign the suspended process to the Job before resuming it;
6. return non-inheritable parent ends as owned safe I/O values;
7. close every temporary handle and terminate/reap every partial child on all failures.

There must be no fallback path that launches an uncontained or non-allowlisted child. ProcessKit should keep the platform `unsafe` inside its reviewed process-creation implementation; Agent24 keeps workspace `unsafe_code = "forbid"`.

POSIX behavior must preserve the existing dedicated process-group ownership and close-on-exec parent endpoints. This dependency is not authorization to redesign process containment.

## Agent24 adoption gate

Agent24 may activate production `run()` only after all of the following are true:

- ProcessKit publishes a pinned release, or an explicitly audited temporary Git revision, with the safe API;
- the sidecar owner consumes that API without raw-handle conversion or new `unsafe`;
- the existing suspended spawn → Job assignment → resume ordering remains tested;
- the host stdio workers receive only owned parent endpoints;
- the three-platform sidecar workflow and binary smoke are green.

The minimum Windows proof must start with an unrelated inheritable sentinel handle, launch a target through the new API, and show that the target cannot access that sentinel. A second test must show that target stdio works: closing the parent's child-stdin writer gives the target EOF, and after the target closes/exits the host observes EOF on the parent-side stdout and stderr readers. Rollback and assignment failures must leave no running child and no leaked pipe or Job handle.

The Agent24 binary smoke must use bounded waits and prove:

- immediate parent EOF exits successfully with empty stdout;
- invalid input returns only fixed, redacted diagnostics;
- a real helper observes `Owned` before `Ready`;
- parent EOF or protocol stop reaches confirmed empty before host exit;
- retained protocol output is flushed before success;
- Windows target and descendant cleanup still uses the same Job authority.

## Dependency order and change cost

The delivery order is N10b → owned host stdio → ProcessKit isolated piped spawn → production `run()` wiring → N10d adverse binary coverage. A green tail PR never permits skipping this order.

Changing timeout budgets later is cheap and local. Replacing blocking files with cancellable I/O is a moderate adapter change. Replacing the containment or Windows spawn primitive is expensive because it requires new rollback, inheritance, descendant, and cross-platform evidence.

N10d may add blocked-output, parent-death, malformed live-control, stderr-flood, and stress coverage. The basic Windows handle allowlist and binary activation smoke are N10c prerequisites, not deferred hardening.
