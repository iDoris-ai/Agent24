# T007 OfficeAssistant Capability and Interaction Research

Status: v0.2 full research draft, incorporating the original page plan; not implemented or acceptance-tested. Updated: 2026-10-16 (UTC). [中文版](report.zh-CN.md)

**Reading baseline**: This draft retains the capability, interaction, technical, permission, and acceptance research from v0.1. Implementation status still comes from the fixed source snapshots reviewed on 2026-10-04 (Agent24 `54cec44`, Components `667809a`), not a fresh code audit on 2026-10-16. Implementation descriptions below, such as “already available,” “not yet wired up,” “default,” and “current run,” must be understood against that historical baseline and rechecked before implementation. This update adds the author's page plan, clarifies product boundaries, and expands the authorization model and demonstration acceptance criteria. It also provides a complete, content-equivalent English report; it does not claim that any functionality has shipped.

## Conclusion

OfficeAssistant is proposed as the assistant capability layer within Agent24's basic office suite: it translates user intent into inspectable plans, drafts, actions, and receipts, reusing Agent24's execution, tools, approvals, audit, and memory systems. Research should first cover one complete chain, “meeting transcript → minutes → action items → confirmation → delivery,” then reuse that structure for email, documents, and knowledge retrieval.

The 2026-10-04 baseline indicates that the most important missing piece is an observable, end-to-end task loop. Components already has an entry point for ordinary users, descriptions of five workflows, and five Skills; the workspace server examined in that review does not yet expose execution endpoints for those Assistant workflows. Agent24 already provides foundations such as runs, tool approvals, cancellation, and status events, but resource authorization, workspace binding, and outbound controls still have gaps in the current product path. Documentation pages and interface infrastructure cannot be combined into a delivery claim that “the office assistant can already execute autonomously.” [S01][S02][S08]

This report defines capability boundaries, interaction proposals, technical trade-offs, and a validation path only; it does not change development priorities. The Agent24 sources reviewed on 2026-10-04 explicitly identify memory M1/P0 as the main track, with P1 immediately following. Open Design was integrated at M10; the subsequent OD-M11/TPI-W/OC tracks were paused. OpenCreator is a planned integration under the author's direction. This topic does not reopen independent research into either creative tool. [S09]

### How the original page plan is incorporated

The author-provided page proposes the promise: “Hand off the repetitive work. Before each task is completed, it will wait for your approval.” This is suitable as a user-facing statement of trust, but it needs an interaction and permission contract that can be tested. The precise definition proposed in this research is: **The assistant first prepares results for review. External sending, remote changes, or expansion of access are performed under the confirmation for that action, or an explicitly granted scope that remains valid; anything outside that scope requires another question.** Reading, organizing, and drafting already requested by the user need not interrupt them at every internal step. Reading itself remains subject to access rights and constraints on where the material may be processed. The original page's broad wording can be retained as historical planning, but a public release should include this explanation so it does not imply that every low-risk step must open a confirmation dialog or that previously granted scope ceases to apply.

The five workflow categories were already covered in v0.1. The addition in this integration is not another feature list, but a mapping from “what it can do” to inputs, reviewable outputs, authorization boundaries, receipts, and validation order. Outbound email, community bulk messaging, Notion read/write access, and LINE are explicitly listed as capabilities requiring configuration and authorization. An introduction page, a Skill file, or a successful connection is neither actual authorization nor proof of production readiness. [S21]

The meeting workflow is the **priority demonstration**: synthetic transcripts or authorized, de-identified samples can validate the structure of “understanding → draft → human edits → confirmation boundary → delivery/receipt” without real customer data. This lowers the barrier to the first validation; it does not claim that the other four workflows cannot be demonstrated. If no real service is connected, the interface must label delivery as simulated. A local demonstration must not be described as a completed Notion or email delivery loop.

## 1. Product Position and Conceptual Boundaries

### Structure already confirmed by the author

- Agent24 itself is the basic Office Suite; everyday office capabilities are part of its foundational positioning.
- Sin90 and Cos72 are OSs loaded for personal or organizational scenarios.
- Capabilities such as email and Notion may be provided by in-house or composed foundational modules.
- Early descriptions of Workspace emphasized multi-action workflows. T004 subsequently clarified it as an existing application's standalone UI plus its complete workflow, embedded into the suite through a menu, with optional or recommended integration. Foundational modules are tool actions callable by the host; they do not require opening that application's interface first. [S22]
- Open Design is already integrated, and OpenCreator is planned for integration; their creative scope may overlap. [S17][S18]

Whether OfficeAssistant ultimately becomes a menu label, module name, or separate directory remains open. Its responsibility should first be defined as “understand tasks, organize steps, present confirmations, and consolidate results.” Drafting email or processing documents should not require Sin90 or Cos72 to be loaded by default. Cos72 can supply organization-specific semantics such as shared materials and member responsibilities; Sin90 can supply personal context. This is a recommendation based on the confirmed product structure, not a claim that either integration is complete.

### Distinct meanings of Workspace

In this report, an “office workflow” is a way to organize a task; the product Workspace is an entry point hosting a standalone application UI and workflow; Agent24's L2 workspace is a filesystem-root, lease, and lifecycle boundary. There are also historical uses of the name for the Cos72 community workbench and organizational memory scopes. These concepts may be related, but a shared name must not imply shared permissions, lifecycles, or state machines. [S09][S22]

The proposed assistant task interface should use “task/workflow,” file-location settings should use “file workspace,” and standalone application entries should retain their specific application names. A background tool call should not be packaged as a workflow that requires opening Workspace. The backend should use distinct IDs, such as workflow_id, run_id, workspace_id, and artifact_id, with explicit relationships rather than merging them. These fields are a proposed data contract, not a claim about existing wire-level interfaces.

### System relationships

| Component | Responsibility toward OfficeAssistant | Boundary to retain at this stage |
|---|---|---|
| Agent24 | The sole execution control plane, owning runs, tools, approvals, audit, and workspace permissions | The assistant must not create a separate execution channel that bypasses the kernel |
| Foundational capability modules/connectors | Bounded actions such as reading email, retrieving materials, and writing pages | Installation, connection, reading, writing, and ongoing authorization must be disclosed separately |
| Documenting | Reading, targeted editing, review, versioning, and export services shared by the assistant and native document interfaces | Covers only the agreed subset of office operations; product-level Office import/export and round-trip behavior require separate acceptance testing, without seeking to replace the full Office suite |
| Knowledge Base / Media Base | Retrieval, provenance, version, and material-processing services within authorized scope | Does not replace the authoritative document originals or revision system; default context or successful indexing does not imply access or publishing permission |
| Application Workspace | A standalone application UI and complete workflow that may consume assistant-generated artifacts | Must not be a mandatory prerequisite for foundational tool calls or become a second approval authority |
| Skill | Input/output rules, steps, quality constraints, and applicability conditions | Text instructions alone neither grant permission nor prove that a connector is available |
| Sin90/Cos72 | Personal/organizational scenario semantics and related data | Basic office work must not require an OS; cross-scope access requires authorization |
| iDoris | The intended model gateway and outbound privacy enforcement layer | Agent24's iDoris provider is not yet wired up; a unified privacy guarantee cannot be claimed |
| AgentEar | An optional voice entry point and source of transcript material | The first iteration can accept transcripts directly, avoiding a complete voice pipeline as a prerequisite |
| Open Design/OpenCreator | Workflow consumers or steps when creative deliverables are needed | Define task and artifact handoffs only; do not expand their development under this topic |
| Hyphae/Web3 | Possible future collaboration, communication, or identity capabilities | This MVP does not depend on or initiate them, and makes no integration promise on that basis |

The historical architecture baseline comes from Agent24's own repository; Components supplies navigation and cross-component context. [S08][S09] Responsibilities for Documenting/KB and application Workspace are aligned with the subsequent T004/T005/T006 research. [S22][S23][S24] “Built in by default and callable by the host” describes the user experience; it does not pre-decide whether Documenting is implemented in-process, out-of-process, or within a domain OS. Its engineering contracts still need to be finalized within its own topic.

## 2. Capability Scope

### Proposed first-version deliverables

1. Turn meeting materials into minutes and action items. Prioritize existing transcripts as input, producing minutes and structured tasks. Each item retains a source_quote/material location; missing owners or dates are marked “needs confirmation.” Commitments not expressed in the source must not be filled in automatically.
2. Turn email or messages into reply drafts. Show the incoming message being answered, the proposed sending account, recipients, attachments, and exact body. Sending is a separate action.
3. Turn documents into action items. Extract requirements, deadlines, and next steps from the specified materials and show the evidence. Third-party instructions inside a document are not authorization.
4. Turn research materials into structured summaries. Bound the source scope and retain links, dates, and uncertainty. Successful summarization must not be treated as verification of external facts.
5. Turn LINE inquiries into suggested replies. The first phase supports manual copying; channel and business-system connections are later extensions under additional authorization.

The first four categories plus LINE match the five existing workflow descriptions; their detailed interactions and success criteria are proposals in this report. The five existing Skills do not map one-to-one to the five workflows: the Skill directories are meeting-to-tasks, mail-draft, line-reply, notion-write, and kb-ingest. It would be incorrect to claim that each workflow already has its own execution implementation. [S02][S03][S04][S05][S06][S07]

### Inputs, outputs, and external actions for the five workflows

| Workflow on the page | Preparation that can be completed first | Subsequent actions requiring explicit authorization | Inspectable results |
|---|---|---|---|
| Meeting → minutes → tasks | Synthetic/authorized transcripts, minutes, decisions, and candidate tasks with provenance; missing owners/dates remain pending confirmation | Write to the specified task system/Notion, or send minutes to specified people | Local artifact versions; real writes additionally produce destination links, external IDs, and result status |
| Email / messages → draft replies | Read the specified authorized thread, create a reply draft, and list references and attachments | Saving a draft in an email service, sending/replying, adding recipients, or attaching new material are each evaluated against the authorized scope | Draft and send receipts are separate; provider acceptance does not mean the recipient has received the message |
| Documents → action items | Extract requirements, deadlines, owners, and source locations from the agreed materials | Modify originals, submit new revisions, synchronize remote tasks, or share files | Source version, fields needing confirmation, change diffs, and corresponding submission receipts |
| Research → structured summaries | Form questions, evidence, conclusions, uncertainties, and next steps within selected public/authorized sources | Use new private materials in queries to third parties, share/publish, or write to a destination repository | Source links and dates, separation of facts from inferences, and missing evidence; summary completion is separate from publication completion |
| LINE inquiries → suggested replies | User-provided or authorized inquiries plus authorized knowledge materials, producing suggestions that can be copied manually | Connect a specified Official Account, read webhook events, and authorize reply/push/broadcast actions separately by scope | Suggested text and supporting evidence; once real sending is enabled, record destinations, message IDs/server-side results, and any unknown delivery status |

“Can be completed first” still presupposes the user's request and permission to access/process the materials. It does not mean automatically reading an entire mailbox, all Notion pages, or every LINE conversation. A draft saved on an external platform is itself a remote write and must not be confused with a local preview. Section 5 defines the specific contracts for the four authorization-sensitive capabilities.

LINE's value as a channel in Thailand can support the choice of scenario, but its user count must not be treated directly as the number of reachable customers. LINE Thailand's official announcement on 2025-03-04 reported **56 million MAU as of June 2024**; a separate official LY Corporation article dated 2025-05-30 recorded 54 million users as of December 2024. Neither explains the difference. This report therefore cites historical figures only with their dates and measurement definitions; it does not treat “56 million users in Thailand” as a real-time 2026 figure or a conclusion about the product's market size. [S25][S26]

### What the first iteration does not take on

It does not build a new email client, Notion-style collaborative editor, CRM, or complete project-management product. Artifacts should be returned to existing tools wherever possible. The first iteration does not enable bulk sending by default, unattended external sending, cross-organization data sharing, automatic payments, or high-risk decisions. Recurring tasks and ongoing authorization must be designed as separate features; a single “agree” must not implicitly enable them.

The KB needs an explicit access scope as a place for reading and delivering material. The current prototype fixes visibility to public, and the embedding/LLM branches for classification are not connected to real implementations. It cannot be used directly as a permission-aware, multi-user knowledge foundation. The public field does not mean that content has been publicly published, but it also provides no isolation guarantee. [S02][S07]

## 3. Interface and Interaction Expectations

### Requirements already stated explicitly

Components records the user's own request that the ordinary-user home page explain “what it can do and how to get started,” with technical information on a separate page. T004 likewise requires starting from user tasks and distinguishing available, trial, and planned capabilities. The Notion Skill explicitly requires users to see which pages the assistant can access. These directions can be carried forward directly. [S01][S05][S17]

The current materials do not establish that the user has selected a chat-based, card-based, or three-column layout. The following interaction proposal is suitable for review; it must not be described as UI already confirmed by the author.

### Three proposed main views

- Start a task: enter a sentence, add materials or select existing ones, and choose a delivery destination. Show the material scope for this task and “draft only/deliver after confirmation.”
- Task details: a one-sentence goal at the top, steps and current status in the main area, and an artifact preview beside or below them. Users can cancel, change requirements, or inspect actions awaiting confirmation at any time.
- Connections and permissions: show connection status, accessible resources, permitted actions, and expiration/revocation controls by account/service. Put model, cost, log, and other troubleshooting information on a separate technical page.

The ordinary interface should use task-oriented language such as “waiting for your confirmation,” “reading materials,” “draft saved,” and “send result needs verification.” Users should not need to understand providers, tokens, MCP, or state machines.

### Seven semantics that must be observable

| Semantic | What the user sees | What the backend must guarantee |
|---|---|---|
| Plan | Which materials will be read, what will be produced, and which steps will write or send | A fixed plan version; new scope, new destinations, or material content changes require authorization to be reassessed |
| Action | The account, destination, input/diff, and affected scope of one action | Each step has its own action_id, attempt records, and associated run; a natural-language passage is not an execution credential |
| Approval | Approve this step, edit, reject, or stop; the server supplies the available choices | Approval is bound to the specific action and final payload version; expiration, rejection, or disconnection must never default to permission |
| Cancel | Stop requested, stopped, and a list of actions already completed | Cancellation stops future work; it does not claim to undo email already sent or remote pages already written |
| Retry | The failed step, what can safely be retried, and the steps already successful | No blind replay of the whole chain; use idempotency keys or query remote results first, and reconcile unknown outcomes before retrying |
| Status | Running, awaiting confirmation, partially complete, blocked, or finished | UI projections follow authoritative kernel state; query again after disconnection rather than inferring success from local animation |
| Receipt | Artifact links/versions, executed steps, failed or unknown steps, and follow-up actions | Approval, tool return, provider acceptance, and actual delivery are recorded separately |

Agent24 already has a plan-approval interface, tool gates, cancellation, and run transitions, but the complete business semantics above still need implementation and acceptance testing. [S12][S13][S14][S15]

Three points require particular care:

- Plan approval is not unlimited authorization. Even when plan mode unlocks a set of tools, external actions must still pass their own gates according to risk, destination, and actual payload.
- Do not conflate gate and advise. A module's “approved” state describes only the decision dimension; the initial required event has no executed_at, and execution results must be checked separately. An approval event is not enough to show “sent” to the user. [S14]
- Current run terminal states are existing enum values such as completed, failed, and cancelled, and terminal states have no outgoing transitions. “Partially complete/outcome unknown/stopping” may be workflow/connector-layer projections or newly defined contracts. A retry creates a related new attempt rather than reviving a finished run in place. [S13]

### A user flow suitable for review

The user uploads a transcript: “Prepare minutes and to-dos, and show them to me first.”

The assistant presents a plan: “Read this one document and generate minutes and action items, without sending them to anyone yet.” It then lists sources item by item, leaving unclear owners and deadlines blank.

The user corrects a task and asks for it to be written to a particular Notion page. The interface shows the account, exact page, actual changes, content that will leave the local machine, and access scope. The confirmation covers this write; it does not grant access to arbitrary pages or permission for long-term synchronization.

After writing, the interface shows the page link and version/receipt. If the request times out but the page may have been created remotely, the status is “result needs verification,” and the system queries first to prevent duplicates. If other steps already succeeded, it retries only incomplete steps. This is a proposed acceptance-test script; no account was connected and no write was executed in this research round.

## 4. Backend Technology Stack and Options

### Existing reusable foundations

Agent24 is implemented as a Rust daemon. Its dependencies include Tokio and Axum; it exposes REST/WebSocket interfaces, and its store uses SQLite and supports auditing. The desktop shell uses TypeScript, Electron, React, and Vite. Tools, MCP, policy, model router, memory, and related functions are existing kernel services. [S08][S10][S11]

The Components workspace is a Python standard-library HTTP prototype. It is suitable for product explanation, displaying a local KB, and technical experiments. The LangGraph/Postgres strings returned by assistant_status do not establish that those dependencies are deployed. [S01][S02]

### Recommended trade-offs

| Option | Suitable scenarios | Assessment in this report |
|---|---|---|
| Agent24 kernel plus a thin workflow layer | Basic office tasks, single-machine use, and integration with the existing desktop | Preferred research path, reusing permission and execution authority; business steps and UI projections need clearly separated layers |
| Supervised out-of-process modules | OS-specific business logic or workflow services that need independent releases | An option; connect to the kernel through os-proto/callback rather than creating a separate path that bypasses model/tool controls |
| LangGraph plus an independent state store | Complex business workflows with a demonstrated need for graph orchestration and recovery after long waits | A candidate only; first establish the gap that Agent24 cannot meet, avoiding duplicate approvals, runs, and audits |
| Introduce another general-purpose Agent framework to replace the kernel | A full-stack rebuild | Not needed for this topic and inconsistent with the established single-control-plane relationship |

Adding a database merely because the historical prototype mentioned Postgres is therefore not recommended. If workflows initially use SQLite, business-state tables or projections can be added, but an ADR must decide the schema, migrations, restart recovery, and boundary with the main store. The frontend must not maintain a separate authoritative set of approval results.

For models, retain Agent24's provider/router interface. MiniCPM was the historical default direction for Assistant, but this topic should not lock in a specific model version or capability based on old benchmarks. Actual meeting-extraction and multi-step tool evaluations should determine whether a higher model tier is needed. If a local model is unavailable and the user selected “local only,” the task should fail explicitly rather than automatically falling back to the cloud. [S08][S19]

There is also a concrete wiring gap: the existing LocalOnly routing mechanism rejects remote execution, but the main conversation, sub-agents, and REST chat still pass a default TaskProfile. Adding a “local only” toggle without propagating it to the actual calls is insufficient. Guardian is disabled by default, so the presence of Guardian code also does not establish that the product automatically reviews actions by default. [S08]

Connectors may use native APIs/MCP or controlled adapters; the important contracts concern permissions, destinations, idempotency, and receipts. The historical resend-cli does not automatically provide personal-mailbox reading/replying capabilities. Account identity, threaded replies, drafts, send permissions, and provider APIs each require validation. Notion likewise requires verification of the final tool's target resources and write behavior; the words “official MCP” are no substitute for integration acceptance testing. [S04][S05]

### Proposed minimum data contract

Workflow record: goal, source references, plan version, step dependencies, associated run, and user-visible status.
Action record: tool/connector, account reference, exact destination, summary of effective inputs, approval binding, idempotency key, and attempt count.
Artifact record: type, location, version/hash, source evidence, and read/write scope.
Receipt record: business outcome, external ID/link, confirmation time, whether compensation is possible, and verification status.
Sensitive body content stays in controlled artifact storage; ordinary events and audit records contain only necessary metadata. Binding a payload by hash does not imply that its full body should be written to logs.

These are OfficeAssistant contract fields still to be finalized, not APIs already supplied by Agent24.

## 5. Permission and Reliability Boundaries

### Four capabilities requiring explicit authorization

The following is a proposed product contract, not authorization from this research to connect or send through any real account. Platform credentials establish technical identity only; they do not replace the user's business authorization.

| Capability | Minimum authorization scope and what the user must see | Boundary between preparation and execution |
|---|---|---|
| Outbound email | Sending account, specific recipients/bounded recipient set, purpose, final body, and attachments; ongoing authorization separately specifies duration, frequency/volume limits, and revocation method | Distinguish local drafting, saving a remote draft, and sending; reassess scope if the account, audience, or substantive content changes |
| Community bulk messaging | Channel, specific community/group and recipient-list snapshot, recipient count, content version, timing, frequency/cost limits, and cancellation cutoff | Separate preview/test sends from full sends; permission to reply to one person does not imply bulk-send permission; retain batch-by-batch confirmation and deduplicated receipts in the first iteration |
| Notion read/write access | Selected connection type, pages/databases and inherited child-page scope; list read/insert/update capabilities separately, with explicit write destinations and diffs | Already authorized reading need not be reconfirmed every time; authorize new scope first; writes use action-specific or valid scoped authorization, and a connection does not permit writing to arbitrary pages |
| LINE | Specified Official Account/channel, readable conversation events, reply/push/broadcast actions, target user/group scope, validity period, and quotas | Suggested replies are manually copyable by default; connecting to receive messages does not authorize sending; automatic replies, proactive pushes, and broadcasts require separately configured business authorization |

Notion's official documentation defines read/update/insert and other capabilities separately; sharing a parent page with a connection includes the corresponding child resources. The interface must show the effective scope, not just the parent page's name. Internal connections require pages to be shared explicitly; public OAuth connections use a page-selection flow. A personal access token's scope is tied to the user's workspace permissions, so not all credential types can be presented as inherently offering page-level least privilege. The final integration method remains to be chosen and acceptance-tested. [S29][S30]

LINE Messaging API replies use a short-lived, single-use reply token from a webhook. After human review or lengthy generation, the token cannot be assumed to remain usable. Push messages also have recipient-eligibility restrictions; broadcasts target friends of that Official Account, not all LINE users. If a reply is no longer possible after a delay, the interface should explain that the original reply route cannot be used. A push may be performed separately only when both eligibility and authorization are satisfied. Push/broadcast messages are subject to plan quotas; HTTP 200 does not establish final delivery. A user's “agree to send” must not be expanded into permission to change channels, expand the audience, or retry without limits. [S27][S28]

### Balance convenience and safety through visible authorization levels

The proposed default is “prepare only / confirm consequential actions individually.” After validation, “bounded delegation” can be added: the user explicitly selects tools, accounts, destinations/audiences, action categories, material/processing scope, validity period, and usage limits, and can pause or revoke authorization at any time. The interface should explain which authorization is being used now, why this action does not need another question, and where its result can be found. Expiration, revocation, out-of-scope actions, or substantive changes to the final payload should pause execution rather than silently switching to another channel.

Requiring a click for every action need not be a permanent product limitation, but “more convenient” cannot mean unrestricted delegation. The first MVP still uses individual confirmations to validate the gates for external actions. Ongoing authorization is a separate later design and acceptance item. Expanding permissions, sending sensitive materials externally, and irreversible/high-risk actions must also follow applicable platform policies and the security requirements of the actual deployment; an ordinary “automatic” setting cannot override those limits. This section proposes a choice mechanism, not an implemented feature.

### Inherited safety and reliability requirements

1. Single authority. The frontend, Skills, MCP descriptions, and external documents cannot expand their own permissions. The kernel still adjudicates every tool call; an installer must not implicitly receive an exemption from risk controls. [S12][S16]
2. Current product authorization limits. The default legacy bearer has full authority. Capability-token infrastructure exists, but route-level resource authorization, event filtering, and related work are incomplete. The desktop's general-purpose proxy also has isolation work outstanding. The first iteration can therefore validate trusted local materials and mock connectors, but should not expose an execution surface for untrusted plugins or organizational sharing. [S08]
3. File safety. Workspace-bound path constraints apply only on the corresponding execution paths, and gaps remain in the default product path. A fixed cwd for shell_exec is not an OS sandbox. Connector execution must not be replaced with arbitrary shell commands to bypass model and permission boundaries. [S08][S09]
4. Govern outbound paths separately. Payloads sent to an LLM and direct outbound email, Notion, HTTP, and MCP traffic are different exits. Local inference does not guarantee that materials stay local. With iDoris not yet wired up and non-LLM egress-guard work still outstanding, end-to-end redaction must not be promised as active. [S09]
5. Approve the final effective payload. Perform necessary privacy processing first, then show the user the exact content to be sent. If recipients, pages, attachments, or substantive body content change after approval, the previous approval cannot be silently reused. Redaction must not unilaterally damage the business meaning; if both needs cannot be satisfied, block the action and explain.
6. Minimum destination authorization. The initial MVP confirms external writes individually. If ongoing authorization is supported later, model it by tool, specific destination, action category, validity period, and revocation scope. A single agreement must not become permission for arbitrary external sending throughout a conversation. Agent24's 2026-10-04 baseline already includes targeted external authorization rules, but some approval-law constraints still need mechanical verification. [S16]
7. Recoverable does not mean retractable. Prefer saved drafts and versioned writes; retain separate gates for sending, bulk messaging, and irreversible operations. Cancellation and compensation are different, and compensation may itself need authorization.
8. Events are not proof of delivery. WebSocket connections can drop; clients must read an authoritative snapshot after reconnecting. The current seq is monotonic per connection and must not be treated as a reliable global cursor across reconnects. [S14]

## 6. Research MVP and Acceptance

### R0: Finalize the experience and contracts

Use synthetic meeting transcripts, mock connectors, and existing Agent24 contracts to design task details, approvals, disconnection, cancellation, and receipts. The demonstration package should explicitly identify samples as synthetic and contain no real customer material, real credentials, or unauthorized accounts. Distinguish simulated and real actions in the interface. Produce a state mapping and a list of required new fields. The success criterion is that reviewers can identify “what will happen, when external effects will occur, and what will remain if we stop now.”

### R1: Complete the local-material workflow

Start with one transcript and complete minutes, action items, source references, user edits, and local delivery. Do not invent owners/dates; explicitly mark items without evidence as needing confirmation. Retain at least one reproducible input, expected minutes/tasks, source locations, and actual output. Check task omissions, unsupported commitments, and preservation of human edits item by item; an attractive page is no substitute for correct results. Test cancellation during reading, model waiting, approval waiting, and at the write boundary, as well as state recovery after restart.

### R2: One authorized connector

The author selects one email or Notion test destination. Begin with read-only access or drafts, then perform one write under explicit authorization. This report does not choose the first connector on the user's behalf. Acceptance requires the actual account scope, controlled test materials, result queries, and evidence of deduplication. A research report is not connection authorization.

### R3: Reuse the structure for a second workflow

Reuse the same action/approval/receipt structure for email drafts or document action items. If the second workflow still requires rewriting approval and recovery mechanisms, the abstraction has not yet been established. Voice, OS-shared materials, and multi-person collaboration can be evaluated afterward.

### Minimum negative-case acceptance matrix

| Scenario | Required observable result |
|---|---|
| User rejects, approval expires, or no approval channel is available | No external action occurs; status is explicit and explainable |
| Destination or final payload changes after approval | The old approval does not permit the new action |
| External service succeeds but the local request times out | Query/reconcile first; no duplicate email or page |
| First step succeeds and second step fails | Preserve the successful receipt and retry only necessary steps |
| Cancellation during execution | Later steps stop; external effects that already occurred are listed truthfully |
| Restart or WS disconnection | Restore authoritative state; do not send again based on an old card |
| Materials contain instructions exceeding authorization | Treat them only as material; do not expand read or send scope |
| Local model is unavailable | A local-only task fails/waits without silently switching to an external service |
| Attempt to access an unauthorized page or path | Reject; permissions cannot be inferred from a workspace with the same name |
| Outbound traffic through a connector bypasses the privacy layer | Fail the test and prevent that execution path from being enabled |
| Bounded delegation is revoked/expired, or the bulk-message recipient list changes | Pause relevant actions and recheck authorization; do not continue sending under the old plan |
| LINE reply token is no longer usable after review | Do not blindly retry or switch to push without authorization; explain the restriction and check eligibility and authorization for the alternative |
| Notion parent-page authorization makes child pages accessible | Show and verify the effective inherited scope; revocation must also invalidate retrieval/writing |
| Mock connector returns success | Clearly label it as a demonstration result; do not report real remote writes or delivery |

Record pass/fail, evidence, and reproduction steps for each scenario. Safety counterexamples must not be obscured by an average success rate. The work on 2026-10-04 was a static source-code review; the work on 2026-10-16 was document integration and verification against official connector documentation. Neither round executed the acceptance tests above.

## 7. A Few Decisions for the Author

- Who is the first version for: personal use or internal collaboration at 4seas? The answer directly affects the scope of shared permissions.
- Prioritize a meeting-workflow skeleton demonstration without customer data. For the first real external end-to-end flow, should it use email or Notion? Which account and exact test destination?
- Should “Assistant” be an entry point on the basic home page, or should tasks be routed through a unified input box? Two views can initially be reviewed using the same interaction contract.

- How should the experience balance individual confirmation by default and later bounded delegation? Are community bulk messaging and automatic LINE sending part of the initial release, or should they remain drafts/manual copying at first?

These decisions do not prevent further refinement of a purely local research draft, but should be settled before connecting accounts or starting development. The MVP research sequence does not reschedule the projects. The historical memory priority and paused tracks still need confirmation against the latest decisions in the relevant projects; this report does not resume them on its own.

## Sources

See [source-collection.md](source-collection.md) for fixed versions, review scope, and handling of conflicting evidence. The links below support item-by-item rechecking; they do not imply runtime validation.

[S01]: https://github.com/iDoris-ai/iDoris-Components/blob/667809ae8557e285133d0c8bd8f6a7401aae41e8/solution/workspace/README.md
[S02]: https://github.com/iDoris-ai/iDoris-Components/blob/667809ae8557e285133d0c8bd8f6a7401aae41e8/solution/workspace/server.py
[S03]: https://github.com/iDoris-ai/iDoris-Components/blob/667809ae8557e285133d0c8bd8f6a7401aae41e8/solution/workspace/data/skills/meeting-to-tasks/SKILL.md
[S04]: https://github.com/iDoris-ai/iDoris-Components/blob/667809ae8557e285133d0c8bd8f6a7401aae41e8/solution/workspace/data/skills/mail-draft/SKILL.md
[S05]: https://github.com/iDoris-ai/iDoris-Components/blob/667809ae8557e285133d0c8bd8f6a7401aae41e8/solution/workspace/data/skills/notion-write/SKILL.md
[S06]: https://github.com/iDoris-ai/iDoris-Components/blob/667809ae8557e285133d0c8bd8f6a7401aae41e8/solution/workspace/data/skills/line-reply/SKILL.md
[S07]: https://github.com/iDoris-ai/iDoris-Components/blob/667809ae8557e285133d0c8bd8f6a7401aae41e8/solution/workspace/data/skills/kb-ingest/SKILL.md
[S08]: https://github.com/iDoris-ai/Agent24/blob/54cec44a7410532e8a864ea90860f2d442c06ba2/docs/ARCHITECTURE-LAYERS.md
[S09]: https://github.com/iDoris-ai/Agent24/blob/54cec44a7410532e8a864ea90860f2d442c06ba2/docs/agent/COMPONENT-ROADMAP.md
[S10]: https://github.com/iDoris-ai/Agent24/blob/54cec44a7410532e8a864ea90860f2d442c06ba2/rust/apps/agent24d/Cargo.toml
[S11]: https://github.com/iDoris-ai/Agent24/blob/54cec44a7410532e8a864ea90860f2d442c06ba2/apps/desktop/package.json
[S12]: https://github.com/iDoris-ai/Agent24/blob/54cec44a7410532e8a864ea90860f2d442c06ba2/rust/crates/agent24-tools/src/lib.rs
[S13]: https://github.com/iDoris-ai/Agent24/blob/54cec44a7410532e8a864ea90860f2d442c06ba2/rust/crates/agent24-core/src/transitions.rs
[S14]: https://github.com/iDoris-ai/Agent24/blob/54cec44a7410532e8a864ea90860f2d442c06ba2/protocol/events.schema.json
[S15]: https://github.com/iDoris-ai/Agent24/blob/54cec44a7410532e8a864ea90860f2d442c06ba2/rust/apps/agent24d/src/approvals.rs
[S16]: https://github.com/iDoris-ai/Agent24/blob/54cec44a7410532e8a864ea90860f2d442c06ba2/docs/laws/APPROVAL.md
[S17]: https://github.com/jhfnetboy/researcher/blob/573250a544490364650542cb63b058652ab2a629/topics/T004-ecosystem-overview/user-side.md
[S18]: https://github.com/jhfnetboy/researcher/blob/573250a544490364650542cb63b058652ab2a629/topics/T004-ecosystem-overview/product-side.md
[S19]: https://github.com/iDoris-ai/iDoris-Components/blob/667809ae8557e285133d0c8bd8f6a7401aae41e8/docs/00-context-and-decisions.md
[S20]: https://github.com/iDoris-ai/iDoris-Components/blob/667809ae8557e285133d0c8bd8f6a7401aae41e8/docs/06-placement-on-agent24.md

[S21]: source-collection.md#s21-作者提供的原页面规划2026-10-16
[S22]: https://github.com/jhfnetboy/researcher/blob/5cb0ace0e1d46877ec174fde2da729f6c415a920/topics/T004-ecosystem-overview/product-side.md
[S23]: https://github.com/jhfnetboy/researcher/blob/5cb0ace0e1d46877ec174fde2da729f6c415a920/topics/T005-documenting/README.md
[S24]: https://github.com/jhfnetboy/researcher/blob/5cb0ace0e1d46877ec174fde2da729f6c415a920/topics/T006-knowledge-media-base/service-boundaries.md
[S25]: https://linenewsroom.com/en/press-release/line-conference-thailand-2025
[S26]: https://www.lycorp.co.jp/en/story/20250530/lineconference.html
[S27]: https://developers.line.biz/en/reference/messaging-api/nojs/
[S28]: https://developers.line.biz/en/docs/messaging-api/pricing/
[S29]: https://developers.notion.com/reference/capabilities
[S30]: https://developers.notion.com/guides/get-started/authorization
