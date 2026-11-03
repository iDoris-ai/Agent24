# R1 附录：v1 路由 → 代码 owner 全表

索引：[R1 模块关系图](R1-MODULE-MAP.md)・[架构总入口](../README.md)

- **数据来源**：`protocol/openapi.yaml`（104 个 path、124 个 operation）；agent24d / agent24-comm / agent24-documents 中生产函数里的 axum `.route()`；`rust/apps/agent24d/src/capabilities/route_policy.rs`。
- **生成方式**：`python3 -I scripts/rust_routes.py <repo> table`（`scripts/` 与本文同目录）生成，crate/module 列是人工归类；只做静态扫描，没有起 daemon。
- **HEAD**：`9865d7a0303cf73e2766e2f20a174159dc1bd975`
- 「policy」列是 `route_policy.rs` 中的 Operation/级别及行号；documents 模块路由不逐条列 policy，经 `decide()` 统一映射为 ModuleRoute/Everyday（route_policy.rs:231-242）。「—(not in openapi)」表示代码注册了但 OpenAPI 里没有该方法。

| # | method | route | openapi tag / opId | owner file:line | handler | crate/module | policy |
|---|---|---|---|---|---|---|---|
| 1 | GET | `/api/v1/approvals` | approvals / listApprovals (L1211) | rust/apps/agent24d/src/server.rs:1565 | `crate::approvals::list_approvals` | agent24d kernel | ApprovalStatusRead/Everyday L101 |
| 2 | GET | `/api/v1/approvals/{id}` | —(not in openapi) | rust/apps/agent24d/src/server.rs:1566 | `crate::approvals::get_approval` | agent24d kernel | ApprovalStatusRead/Everyday L102 |
| 3 | POST | `/api/v1/approvals/{id}` | approvals / decideApproval (L1237) | rust/apps/agent24d/src/server.rs:1566 | `crate::approvals::decide_approval` | agent24d kernel | ApprovalDecision/Elevated L103 |
| 4 | GET | `/api/v1/attached` | attached / listAttachedModules (L824) | rust/apps/agent24d/src/server.rs:1609 | `crate::attached_routes::list_attached` | agent24d kernel | ModuleRead/Everyday L117 |
| 5 | POST | `/api/v1/attached` | attached / registerAttachedModule (L824) | rust/apps/agent24d/src/server.rs:1609 | `crate::attached_routes::post_attached` | agent24d kernel | ModuleAdmin/Elevated L116 |
| 6 | DELETE | `/api/v1/attached/{name}` | attached / revokeAttachedModule (L874) | rust/apps/agent24d/src/server.rs:1613 | `crate::attached_routes::delete_attached` | agent24d kernel | ModuleAdmin/Elevated L118 |
| 7 | PATCH | `/api/v1/attached/{name}` | attached / updateAttachedModule (L874) | rust/apps/agent24d/src/server.rs:1613 | `crate::attached_routes::patch_attached` | agent24d kernel | ModuleAdmin/Elevated L119 |
| 8 | POST | `/api/v1/capabilities/creative` | —(not in openapi) | rust/apps/agent24d/src/server.rs:1487 | `mint_creative_capability` | agent24d kernel | CapabilityMint/Elevated L80 |
| 9 | POST | `/api/v1/capabilities/{capability_id}/revoke` | —(not in openapi) | rust/apps/agent24d/src/server.rs:1491 | `revoke_capability` | agent24d kernel | CapabilityRevoke/Elevated L81 |
| 10 | POST | `/api/v1/chat` | chat / postChat (L463) | rust/apps/agent24d/src/server.rs:1481 | `crate::routes::post_chat` | agent24d kernel | ChatCreate/Everyday L77 |
| 11 | GET | `/api/v1/comm/contact` | comm / listCommContacts (L3084) | rust/crates/agent24-comm/src/router.rs:174 | `list_contact` | agent24-comm | CommRead/Everyday L149 |
| 12 | POST | `/api/v1/comm/contact` | comm / addCommContact (L3084) | rust/crates/agent24-comm/src/router.rs:174 | `add_contact` | agent24-comm | CommWrite/Elevated L150 |
| 13 | GET | `/api/v1/comm/daemon` | comm / getCommDaemon (L3325) | rust/crates/agent24-comm/src/router.rs:178 | `daemon_status` | agent24-comm | CommRead/Everyday L155 |
| 14 | POST | `/api/v1/comm/daemon/start` | comm / startCommDaemon (L3352) | rust/crates/agent24-comm/src/router.rs:179 | `daemon_start` | agent24-comm | CommWrite/Elevated L156 |
| 15 | POST | `/api/v1/comm/daemon/stop` | comm / stopCommDaemon (L3373) | rust/crates/agent24-comm/src/router.rs:180 | `daemon_stop` | agent24-comm | CommWrite/Elevated L157 |
| 16 | GET | `/api/v1/comm/groupchat` | comm / listCommGroups (L3579) | rust/crates/agent24-comm/src/groupchat.rs:61 | `list_groups` | agent24-comm | CommRead/Everyday L164 |
| 17 | POST | `/api/v1/comm/groupchat` | comm / createCommGroup (L3579) | rust/crates/agent24-comm/src/groupchat.rs:61 | `create_group` | agent24-comm | CommWrite/Elevated L165 |
| 18 | POST | `/api/v1/comm/groupchat/invites/{invite_id}/accept` | comm / acceptCommGroupInvite (L3859) | rust/crates/agent24-comm/src/groupchat.rs:73 | `accept` | agent24-comm | CommWrite/Elevated L172 |
| 19 | GET | `/api/v1/comm/groupchat/{group_id}/invites` | comm / listCommGroupInvites (L3679) | rust/crates/agent24-comm/src/groupchat.rs:63 | `invites` | agent24-comm | CommRead/Everyday L167 |
| 20 | GET | `/api/v1/comm/groupchat/{group_id}/members` | comm / listCommGroupMembers (L3647) | rust/crates/agent24-comm/src/groupchat.rs:62 | `members` | agent24-comm | CommRead/Everyday L166 |
| 21 | GET | `/api/v1/comm/groupchat/{group_id}/messages` | comm / listCommGroupMessages (L3711) | rust/crates/agent24-comm/src/groupchat.rs:64 | `history` | agent24-comm | CommRead/Everyday L168 |
| 22 | POST | `/api/v1/comm/groupchat/{group_id}/messages` | comm / sendCommGroupMessage (L3711) | rust/crates/agent24-comm/src/groupchat.rs:64 | `send_message` | agent24-comm | CommWrite/Elevated L169 |
| 23 | GET | `/api/v1/comm/groupchat/{group_id}/messages/{logical_id}` | comm / getCommGroupMessageStatus (L3785) | rust/crates/agent24-comm/src/groupchat.rs:68 | `status` | agent24-comm | CommRead/Everyday L170 |
| 24 | POST | `/api/v1/comm/groupchat/{group_id}/messages/{logical_id}/retry` | comm / retryCommGroupMessage (L3818) | rust/crates/agent24-comm/src/groupchat.rs:69 | `retry` | agent24-comm | CommWrite/Elevated L171 |
| 25 | GET | `/api/v1/comm/history` | comm / listCommHistory (L3429) | rust/crates/agent24-comm/src/router.rs:182 | `get_history` | agent24-comm | CommRead/Everyday L159 |
| 26 | GET | `/api/v1/comm/identity` | comm / listCommIdentities (L2770) | rust/crates/agent24-comm/src/router.rs:169 | `list_identity` | agent24-comm | CommRead/Everyday L143 |
| 27 | POST | `/api/v1/comm/identity` | comm / createCommIdentity (L2770) | rust/crates/agent24-comm/src/router.rs:169 | `create_identity` | agent24-comm | CommWrite/Elevated L144 |
| 28 | POST | `/api/v1/comm/identity/default` | comm / useCommIdentity (L3014) | rust/crates/agent24-comm/src/router.rs:170 | `use_identity` | agent24-comm | CommWrite/Elevated L145 |
| 29 | POST | `/api/v1/comm/identity/export` | comm / exportCommIdentity (L2828) | rust/crates/agent24-comm/src/router.rs:171 | `export_identity` | agent24-comm | CommWrite/Elevated L146 |
| 30 | POST | `/api/v1/comm/identity/import` | comm / importCommIdentity (L2926) | rust/crates/agent24-comm/src/router.rs:172 | `import_identity` | agent24-comm | CommWrite/Elevated L147 |
| 31 | POST | `/api/v1/comm/import` | comm / importCommHome (L3236) | rust/crates/agent24-comm/src/router.rs:177 | `import` | agent24-comm | CommWrite/Elevated L154 |
| 32 | POST | `/api/v1/comm/inbox/pull` | comm / pullCommInbox (L3457) | rust/crates/agent24-comm/src/router.rs:183 | `pull_inbox` | agent24-comm | CommWrite/Elevated L160 |
| 33 | POST | `/api/v1/comm/install` | comm / installCommComponent (L3273) | rust/apps/agent24d/src/comm_routes.rs:635 | `install_component` | agent24d comm_routes | ComponentInstall/Elevated L142 |
| 34 | GET | `/api/v1/comm/outbox` | comm / listCommOutbox (L3490) | rust/crates/agent24-comm/src/router.rs:184 | `list_outbox` | agent24-comm | CommRead/Everyday L161 |
| 35 | POST | `/api/v1/comm/outbox/clear` | comm / clearCommOutbox (L3544) | rust/crates/agent24-comm/src/router.rs:186 | `clear_outbox` | agent24-comm | CommWrite/Elevated L163 |
| 36 | POST | `/api/v1/comm/outbox/{event_id}/retry` | comm / retryCommOutbox (L3517) | rust/crates/agent24-comm/src/router.rs:185 | `retry_outbox` | agent24-comm | CommWrite/Elevated L162 |
| 37 | GET | `/api/v1/comm/relay` | comm / listCommRelays (L3143) | rust/crates/agent24-comm/src/router.rs:175 | `list_relay` | agent24-comm | CommRead/Everyday L151 |
| 38 | PUT | `/api/v1/comm/relay` | comm / setCommRelays (L3143) | rust/crates/agent24-comm/src/router.rs:175 | `set_relay` | agent24-comm | CommWrite/Elevated L152 |
| 39 | POST | `/api/v1/comm/relay/probe` | comm / probeCommRelay (L3203) | rust/crates/agent24-comm/src/router.rs:176 | `probe_relay` | agent24-comm | CommWrite/Elevated L153 |
| 40 | POST | `/api/v1/comm/send` | comm / sendCommMessage (L3390) | rust/crates/agent24-comm/src/router.rs:181 | `send_message` | agent24-comm | CommWrite/Elevated L158 |
| 41 | POST | `/api/v1/comm/unlock` | comm / unlockComm (L3048) | rust/crates/agent24-comm/src/router.rs:173 | `unlock` | agent24-comm | CommWrite/Elevated L148 |
| 42 | GET | `/api/v1/components` | components / listComponents (L496) | rust/apps/agent24d/src/server.rs:1453 | `crate::component_routes::list` | agent24d kernel | ComponentsRead/Everyday L67 |
| 43 | POST | `/api/v1/components/{id}/report` | components / reportComponentState (L523) | rust/apps/agent24d/src/server.rs:1454 | `crate::component_routes::report` | agent24d kernel | ComponentsReport/Everyday L68 |
| 44 | GET | `/api/v1/decide/profile` | system / getDecideProfile (L562) | rust/apps/agent24d/src/server.rs:1483 | `crate::decide_routes::get_decide_profile` | agent24d kernel | ConfigRead/Everyday L79 |
| 45 | GET | `/api/v1/diagnostics/migration` | system / getMigrationDiagnostics (L401) | rust/apps/agent24d/src/docked_router.rs:129 | `migration` | agent24d docked | DiagnosticRead/Everyday L64 |
| 46 | GET | `/api/v1/diagnostics/migration/snapshots` | system / getMigrationSnapshots (L424) | rust/apps/agent24d/src/docked_router.rs:130 | `snapshots` | agent24d docked | DiagnosticRead/Everyday L65 |
| 47 | GET | `/api/v1/diagnostics/migration/space` | system / getMigrationSpace (L444) | rust/apps/agent24d/src/docked_router.rs:131 | `space` | agent24d docked | DiagnosticRead/Everyday L66 |
| 48 | POST | `/api/v1/documents/_a24/authorization/revoked` | —(not in openapi) | rust/apps/agent24-documents/src/lib.rs:205 | `authorization::revoked` | agent24-documents (OOP module) | — |
| 49 | POST | `/api/v1/documents/_a24/tools/{op}` | —(not in openapi) | rust/apps/agent24-documents/src/lib.rs:204 | `tools::call_tool` | agent24-documents (OOP module) | — |
| 50 | GET | `/api/v1/documents/capabilities` | documents / documentsGetCapabilities (L2276) | rust/apps/agent24-documents/src/lib.rs:172 | `get_capabilities` | agent24-documents (OOP module) | — |
| 51 | GET | `/api/v1/documents/documents` | documents / documentsListDocuments (L2292) | rust/apps/agent24-documents/src/lib.rs:173 | `documents::list_documents` | agent24-documents (OOP module) | — |
| 52 | GET | `/api/v1/documents/documents/{document_id}` | documents / documentsGetDocument (L2312) | rust/apps/agent24-documents/src/lib.rs:174 | `documents::get_document` | agent24-documents (OOP module) | — |
| 53 | POST | `/api/v1/documents/documents/{document_id}/extractions` | documents / documentsExtract (L2672) | rust/apps/agent24-documents/src/lib.rs:184 | `extract::handler::extract` | agent24-documents (OOP module) | — |
| 54 | POST | `/api/v1/documents/documents/{document_id}/find` | documents / documentsFind (L2641) | rust/apps/agent24-documents/src/lib.rs:183 | `read::find::find` | agent24-documents (OOP module) | — |
| 55 | GET | `/api/v1/documents/documents/{document_id}/revisions/{revision}/pages/{page}` | documents / documentsRenderPage (L2540) | rust/apps/agent24-documents/src/lib.rs:179 | `read::render::render_page` | agent24-documents (OOP module) | — |
| 56 | GET | `/api/v1/documents/documents/{document_id}/revisions/{revision}/text` | documents / documentsReadText (L2600) | rust/apps/agent24-documents/src/lib.rs:175 | `read::text::read_text` | agent24-documents (OOP module) | — |
| 57 | GET | `/api/v1/documents/extractions/{extraction_id}` | documents / documentsGetExtraction (L2737) | rust/apps/agent24-documents/src/lib.rs:188 | `extractions::get_extraction` | agent24-documents (OOP module) | — |
| 58 | POST | `/api/v1/documents/imports` | documents / documentsImport (L2420) | rust/apps/agent24-documents/src/lib.rs:192 | `imports::handler::import` | agent24-documents (OOP module) | — |
| 59 | GET | `/api/v1/documents/jobs/{job_id}` | documents / documentsGetJob (L2463) | rust/apps/agent24-documents/src/lib.rs:193 | `jobs::get_job` | agent24-documents (OOP module) | — |
| 60 | POST | `/api/v1/documents/jobs/{job_id}/cancel` | documents / documentsCancelJob (L2486) | rust/apps/agent24-documents/src/lib.rs:194 | `jobs::cancel_job` | agent24-documents (OOP module) | — |
| 61 | POST | `/api/v1/documents/jobs/{job_id}/retry` | documents / documentsRetryJob (L2506) | rust/apps/agent24-documents/src/lib.rs:195 | `jobs::retry_job` | agent24-documents (OOP module) | — |
| 62 | POST | `/api/v1/documents/uploads` | documents / documentsCreateUpload (L2332) | rust/apps/agent24-documents/src/lib.rs:196 | `uploads::create_upload` | agent24-documents (OOP module) | — |
| 63 | POST | `/api/v1/documents/uploads/{upload_id}/chunks` | documents / documentsAppendUploadChunk (L2368) | rust/apps/agent24-documents/src/lib.rs:197 | `uploads::chunks::append_chunk` | agent24-documents (OOP module) | — |
| 64 | GET | `/api/v1/events` | —(not in openapi) | rust/apps/agent24d/src/server.rs:1603 | `crate::events::ws_events` | agent24d kernel | EventsRead/Everyday L115 |
| 65 | GET | `/api/v1/health` | system / getHealth (L377) | rust/apps/agent24d/src/server.rs:1452<br>rust/apps/agent24d/src/docked_router.rs:128 | `health`<br>`health` | agent24d kernel<br>agent24d docked | HealthRead/Public L63 |
| 66 | DELETE | `/api/v1/host/knowledge/bindings/{binding_id}` | —(not in openapi) | rust/apps/agent24d/src/server.rs:1498 | `crate::knowledge_host::change` | agent24d kernel | KnowledgeHost/Elevated L127 |
| 67 | PUT | `/api/v1/host/knowledge/bindings/{binding_id}` | —(not in openapi) | rust/apps/agent24d/src/server.rs:1498 | `crate::knowledge_host::change` | agent24d kernel | KnowledgeHost/Elevated L126 |
| 68 | DELETE | `/api/v1/host/knowledge/bindings/{binding_id}/credential` | —(not in openapi) | rust/apps/agent24d/src/server.rs:1502 | `crate::knowledge_host::revoke_credential` | agent24d kernel | KnowledgeHost/Elevated L129 |
| 69 | PUT | `/api/v1/host/knowledge/bindings/{binding_id}/credential` | —(not in openapi) | rust/apps/agent24d/src/server.rs:1502 | `crate::knowledge_host::push_credential` | agent24d kernel | KnowledgeHost/Elevated L128 |
| 70 | POST | `/api/v1/idoris/connect` | idoris / connectIdoris (L4024) | rust/apps/agent24d/src/server.rs:1463 | `crate::idoris_routes::connect` | agent24d kernel | IdorisLifecycle/Elevated L71 |
| 71 | POST | `/api/v1/idoris/install` | idoris / installIdoris (L3975) | rust/apps/agent24d/src/server.rs:1459 | `crate::idoris_routes::install` | agent24d kernel | IdorisLifecycle/Elevated L70 |
| 72 | GET | `/api/v1/idoris/omlx/candidates` | idoris / getIdorisOmlxCandidates (L3898) | rust/apps/agent24d/src/server.rs:1467 | `crate::idoris_routes::candidates` | agent24d kernel | IdorisLifecycle/Elevated L72 |
| 73 | GET | `/api/v1/idoris/operations/{id}` | idoris / getIdorisOperation (L4163) | rust/apps/agent24d/src/server.rs:1473 | `crate::idoris_routes::operation` | agent24d kernel | IdorisRead/Everyday L75 |
| 74 | POST | `/api/v1/idoris/operations/{id}/cancel` | idoris / cancelIdorisOperation (L4205) | rust/apps/agent24d/src/server.rs:1477 | `crate::idoris_routes::cancel` | agent24d kernel | IdorisLifecycle/Elevated L76 |
| 75 | POST | `/api/v1/idoris/start` | idoris / startIdoris (L4073) | rust/apps/agent24d/src/server.rs:1471 | `crate::idoris_routes::start` | agent24d kernel | IdorisLifecycle/Elevated L73 |
| 76 | GET | `/api/v1/idoris/status` | idoris / getIdorisStatus (L3940) | rust/apps/agent24d/src/server.rs:1458 | `crate::idoris_routes::status` | agent24d kernel | IdorisRead/Everyday L69 |
| 77 | POST | `/api/v1/idoris/stop` | idoris / stopIdoris (L4118) | rust/apps/agent24d/src/server.rs:1472 | `crate::idoris_routes::stop` | agent24d kernel | IdorisLifecycle/Elevated L74 |
| 78 | GET | `/api/v1/memory/assertions` | memory / listMemoryAssertions (L1863) | rust/apps/agent24d/src/server.rs:1552 | `crate::memory_routes::list_assertions` | agent24d kernel | MemoryRead/Everyday L97 |
| 79 | DELETE | `/api/v1/memory/assertions/{id}` | memory / forgetMemoryAssertion (L1918) | rust/apps/agent24d/src/server.rs:1556 | `crate::memory_routes::delete_assertion` | agent24d kernel | MemoryForget/Everyday L98 |
| 80 | GET | `/api/v1/memory/settings` | memory / getMemorySettings (L1974) | rust/apps/agent24d/src/server.rs:1560 | `crate::memory_routes::get_memory_settings` | agent24d kernel | MemoryRead/Everyday L99 |
| 81 | PUT | `/api/v1/memory/settings` | memory / putMemorySettings (L1974) | rust/apps/agent24d/src/server.rs:1560 | `crate::memory_routes::put_memory_settings` | agent24d kernel | MemorySettingsWrite/Elevated L100 |
| 82 | GET | `/api/v1/models` | system / listModels (L584) | rust/apps/agent24d/src/server.rs:1482 | `crate::routes::get_models` | agent24d kernel | ModelsRead/Everyday L78 |
| 83 | GET | `/api/v1/module-approvals` | module-approvals / listModuleApprovals (L1278) | rust/apps/agent24d/src/server.rs:1572 | `crate::module_approvals::list_module_approvals` | agent24d kernel | ApprovalStatusRead/Everyday L104 |
| 84 | GET | `/api/v1/module-approvals/{id}` | module-approvals / getModuleApproval (L1306) | rust/apps/agent24d/src/server.rs:1576 | `crate::module_approvals::get_module_approval` | agent24d kernel | ApprovalStatusRead/Everyday L105 |
| 85 | POST | `/api/v1/module-approvals/{id}` | module-approvals / decideModuleApproval (L1306) | rust/apps/agent24d/src/server.rs:1576 | `crate::module_approvals::decide_module_approval` | agent24d kernel | ApprovalDecision/Elevated L106 |
| 86 | GET | `/api/v1/module-consents` | tools / listModuleConsents (L95) | rust/apps/agent24d/src/server.rs:1514 | `crate::consent_routes::list` | agent24d kernel | ConfigRead/Everyday L86 |
| 87 | GET | `/api/v1/module-consents/export` | tools / exportModuleConsents (L131) | rust/apps/agent24d/src/server.rs:1515 | `crate::consent_routes::export` | agent24d kernel | ConfigRead/Everyday L87 |
| 88 | DELETE | `/api/v1/module-consents/modules/{module}` | tools / revokeModuleConsents (L158) | rust/apps/agent24d/src/server.rs:1519 | `crate::consent_routes::revoke_module` | agent24d kernel | ConsentRevoke/Everyday L88 |
| 89 | DELETE | `/api/v1/module-consents/modules/{module}/tools/{tool}` | tools / revokeModuleToolConsent (L185) | rust/apps/agent24d/src/server.rs:1523 | `crate::consent_routes::revoke_tool` | agent24d kernel | ConsentRevoke/Everyday L89 |
| 90 | POST | `/api/v1/module-consents/modules/{module}/tools/{tool}` | tools / decideModuleToolConsent (L185) | rust/apps/agent24d/src/server.rs:1523 | `crate::consent_routes::decide` | agent24d kernel | ApprovalDecision/Elevated L90 |
| 91 | GET | `/api/v1/module-consents/modules/{module}/tools/{tool}/card` | tools / getModuleToolCard (L315) | rust/apps/agent24d/src/server.rs:1528 | `crate::consent_routes::card` | agent24d kernel | ConfigRead/Everyday L91 |
| 92 | GET | `/api/v1/os` | system / listDomainOs (L603) | rust/apps/agent24d/src/server.rs:1619 | `crate::os_routes::list_os` | agent24d kernel | ModuleRead/Everyday L120 |
| 93 | PATCH | `/api/v1/os/{name}` | system / updateDomainOs (L622) | rust/apps/agent24d/src/server.rs:1624 | `crate::os_routes::patch_os` | agent24d kernel | ModuleAdmin/Elevated L121 |
| 94 | POST | `/api/v1/os/{name}/commands/{command}` | system / invokeAttachedModuleCommand (L754) | rust/apps/agent24d/src/server.rs:1639 | `crate::attach_commands::post_command` | agent24d kernel | ModuleAdmin/Elevated L124 |
| 95 | POST | `/api/v1/os/{name}/delegation` | system / delegateDomainOs (L669) | rust/apps/agent24d/src/server.rs:1620 | `establish_module_delegation` | agent24d kernel | ModuleAdmin/Elevated L122 |
| 96 | POST | `/api/v1/os/{name}/stop` | system / stopDomainOs (L721) | rust/apps/agent24d/src/server.rs:1630 | `crate::os_routes::stop_now_os` | agent24d kernel | ModuleAdmin/Elevated L123 |
| 97 | GET | `/api/v1/peer/runs` | runs / listPeerRuns (L1164) | rust/apps/agent24d/src/server.rs:1659 | `crate::peer_runs::list_peer_runs` | agent24d kernel | RunRead/Everyday L139 |
| 98 | GET | `/api/v1/peer/runs/{id}` | runs / getPeerRun (L1193) | rust/apps/agent24d/src/server.rs:1660 | `crate::peer_runs::get_peer_run` | agent24d kernel | RunRead/Everyday L140 |
| 99 | GET | `/api/v1/presence/challenges/next` | —(not in openapi) | rust/apps/agent24d/src/server.rs:1668 | `crate::presence_desktop::get_next` | agent24d kernel | PresenceRead/Everyday L130 |
| 100 | POST | `/api/v1/presence/challenges/{id}` | —(not in openapi) | rust/apps/agent24d/src/server.rs:1672 | `crate::presence_desktop::post_decision` | agent24d kernel | PresenceDecide/Everyday L131 |
| 101 | GET | `/api/v1/runs` | runs / listRuns (L1064) | rust/apps/agent24d/src/server.rs:1652 | `crate::runs::list_runs` | agent24d kernel | RunRead/Everyday L137 |
| 102 | POST | `/api/v1/runs` | runs / createRun (L1064) | rust/apps/agent24d/src/server.rs:1652 | `crate::runs::create_run` | agent24d kernel | RunCreate/Everyday L136 |
| 103 | GET | `/api/v1/runs/{id}` | runs / getRun (L1124) | rust/apps/agent24d/src/server.rs:1656 | `crate::runs::get_run` | agent24d kernel | RunRead/Everyday L138 |
| 104 | POST | `/api/v1/runs/{id}/cancel` | runs / cancelRun (L1142) | rust/apps/agent24d/src/server.rs:1657 | `crate::runs::cancel_run` | agent24d kernel | RunCancel/Everyday L141 |
| 105 | GET | `/api/v1/schedules` | schedules / listSchedules (L1374) | rust/apps/agent24d/src/server.rs:1581 | `crate::schedules::list_schedules` | agent24d kernel | ScheduleRead/Everyday L107 |
| 106 | POST | `/api/v1/schedules` | schedules / createSchedule (L1374) | rust/apps/agent24d/src/server.rs:1581 | `crate::schedules::create_schedule` | agent24d kernel | ScheduleManage/Elevated L108 |
| 107 | DELETE | `/api/v1/schedules/{id}` | schedules / deleteSchedule (L1420) | rust/apps/agent24d/src/server.rs:1585 | `crate::schedules::delete_schedule` | agent24d kernel | ScheduleManage/Elevated L111 |
| 108 | GET | `/api/v1/schedules/{id}` | schedules / getSchedule (L1420) | rust/apps/agent24d/src/server.rs:1585 | `crate::schedules::get_schedule` | agent24d kernel | ScheduleRead/Everyday L109 |
| 109 | PATCH | `/api/v1/schedules/{id}` | schedules / updateSchedule (L1420) | rust/apps/agent24d/src/server.rs:1585 | `crate::schedules::update_schedule` | agent24d kernel | ScheduleManage/Elevated L110 |
| 110 | POST | `/api/v1/schedules/{id}/resume` | schedules / resumeSchedule (L1605) | rust/apps/agent24d/src/server.rs:1599 | `crate::schedules::resume_schedule` | agent24d kernel | ScheduleManage/Elevated L114 |
| 111 | POST | `/api/v1/schedules/{id}/run_now` | schedules / runScheduleNow (L1533) | rust/apps/agent24d/src/server.rs:1591 | `crate::schedules::run_now` | agent24d kernel | ScheduleManage/Elevated L112 |
| 112 | POST | `/api/v1/schedules/{id}/suspend` | schedules / suspendSchedule (L1569) | rust/apps/agent24d/src/server.rs:1595 | `crate::schedules::suspend_schedule` | agent24d kernel | ScheduleManage/Elevated L113 |
| 113 | GET | `/api/v1/sessions` | sessions / listSessions (L1006) | rust/apps/agent24d/src/server.rs:1647 | `crate::runs::list_sessions` | agent24d kernel | SessionRead/Everyday L134 |
| 114 | POST | `/api/v1/sessions` | sessions / createSession (L1006) | rust/apps/agent24d/src/server.rs:1647 | `crate::runs::create_session` | agent24d kernel | SessionCreate/Everyday L133 |
| 115 | GET | `/api/v1/sessions/{id}` | sessions / getSession (L1046) | rust/apps/agent24d/src/server.rs:1651 | `crate::runs::get_session` | agent24d kernel | SessionRead/Everyday L135 |
| 116 | GET | `/api/v1/shutdown` | system / getShutdownReport (L1652) | rust/apps/agent24d/src/server.rs:1643 | `shutdown_report` | agent24d kernel | ConfigRead/Everyday L132 |
| 117 | POST | `/api/v1/shutdown` | system / shutdownDaemon (L1652) | rust/apps/agent24d/src/server.rs:1643 | `shutdown_handler` | agent24d kernel | Shutdown/Elevated L125 |
| 118 | GET | `/api/v1/sin90/attention` | sin90 / sin90Attention (L2247) | —(no literal route) |  |  | — |
| 119 | GET | `/api/v1/sin90/directions` | sin90 / sin90ListDirections (L2032) | —(no literal route) |  |  | — |
| 120 | POST | `/api/v1/sin90/directions` | sin90 / sin90CreateDirection (L2032) | —(no literal route) |  |  | — |
| 121 | GET | `/api/v1/sin90/proposals` | sin90 / sin90ListProposals (L2153) | —(no literal route) |  |  | — |
| 122 | POST | `/api/v1/sin90/proposals` | sin90 / sin90SubmitProposal (L2153) | —(no literal route) |  |  | — |
| 123 | GET | `/api/v1/sin90/proposals/{id}` | sin90 / sin90GetProposal (L2199) | —(no literal route) |  |  | — |
| 124 | POST | `/api/v1/sin90/proposals/{id}/accept` | sin90 / sin90AcceptProposal (L2218) | —(no literal route) |  |  | — |
| 125 | GET | `/api/v1/sin90/schedule-blocks` | sin90 / sin90ListBlocks (L2074) | —(no literal route) |  |  | — |
| 126 | POST | `/api/v1/sin90/schedule-blocks` | sin90 / sin90CreateBlock (L2074) | —(no literal route) |  |  | — |
| 127 | PATCH | `/api/v1/sin90/schedule-blocks/{id}` | sin90 / sin90TransitionBlock (L2118) | —(no literal route) |  |  | — |
| 128 | GET | `/api/v1/standing-grants` | tools / listStandingGrants (L1722) | rust/apps/agent24d/src/server.rs:1536 | `crate::overrides::list_standing_grants` | agent24d kernel | ConfigRead/Everyday L93 |
| 129 | DELETE | `/api/v1/standing-grants/{id}` | tools / revokeStandingGrant (L1747) | rust/apps/agent24d/src/server.rs:1540 | `crate::overrides::delete_standing_grant` | agent24d kernel | GrantManage/Elevated L94 |
| 130 | GET | `/api/v1/timings` | timings / listTimings (L924) | rust/apps/agent24d/src/server.rs:1508 | `crate::routes::get_timings` | agent24d kernel | UsageRead/Everyday L83 |
| 131 | GET | `/api/v1/timings/summary` | timings / summarizeTimings (L964) | rust/apps/agent24d/src/server.rs:1509 | `crate::routes::get_timings_summary` | agent24d kernel | UsageRead/Everyday L84 |
| 132 | GET | `/api/v1/tool-overrides` | tools / listToolOverrides (L1773) | rust/apps/agent24d/src/server.rs:1532 | `crate::overrides::list_overrides` | agent24d kernel | ConfigRead/Everyday L92 |
| 133 | DELETE | `/api/v1/tool-overrides/{pattern}` | tools / deleteToolOverride (L1797) | rust/apps/agent24d/src/server.rs:1544 | `crate::overrides::delete_override` | agent24d kernel | OverrideManage/Elevated L96 |
| 134 | PUT | `/api/v1/tool-overrides/{pattern}` | tools / setToolOverride (L1797) | rust/apps/agent24d/src/server.rs:1544 | `crate::overrides::put_override` | agent24d kernel | OverrideManage/Elevated L95 |
| 135 | GET | `/api/v1/tools` | tools / listTools (L1702) | rust/apps/agent24d/src/server.rs:1513 | `crate::routes::get_tools` | agent24d kernel | ConfigRead/Everyday L85 |
| 136 | GET | `/api/v1/usage` | system / getUsage (L993) | rust/apps/agent24d/src/server.rs:1507 | `crate::routes::get_usage` | agent24d kernel | UsageRead/Everyday L82 |

sin90 的 10 个 operation（openapi.yaml L2032–2247）在本仓库没有字面路由：由外部包 `iDoris-ai/Sin90` 经 `domain.rs:1452` 动态挂载（见正文 §6.3）。
