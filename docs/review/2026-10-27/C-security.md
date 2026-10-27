# 附录 C：安全（Codex gpt-6.1-sol，只读沙箱，基线 ab/core 4f4f49ab）

> 原始评审稿，仅去掉首行工具提示；主报告中的修正见 CODE-REVIEW-2026-10-27.md §2。


## 总评

基线为 `4f4f49ab1e5013a6ad800186d13d4aebcbc86395`。发现 5 项问题，主要涉及开发后端未鉴权执行、桌面渲染层借用全权令牌、安装前执行及凭据生命周期。未确认 Rust HTTP 的匿名业务接口绕过或 Creative 越权。以下区分代码确认与条件性攻击；未修改文件、运行项目构建、测试或联网。

## 发现清单

### S-01｜High｜CWE-306：Node 后端的模块安装入口未鉴权

**位置**：`packages/node-daemon/src/server.ts:88`、`:167`；`packages/node-daemon/src/module-installer.ts:57`

**攻击场景**：开发者显式运行 Node 参考后端；攻击者能让该用户访问恶意网页，且浏览器允许网页请求回环地址。网页向 `127.0.0.1:8765/api/modules/install` 发送 `text/plain` POST，正文为攻击者 npm 包名的 JSON。服务端不检查 bearer、Host、Origin 或 Content-Type，直接执行安装。攻击者包的安装脚本或入口代码以开发者身份运行。攻击不需要读取 HTTP 响应。

**证据**：

- `handleRequest` 直接分派业务路由，没有鉴权。
- `readBody` 对任意 Content-Type 的正文执行 `JSON.parse`。
- 安装端点调用 `installModule(packageName)`。
- 安装器执行 `npm install <packageName> --save`，没有禁用生命周期脚本。

**建议修补**：开发后端也强制随机 bearer；对非预期 Host、浏览器 Origin 和非 JSON 请求拒绝处理。若只用于契约测试，应移除生产能力安装入口。

**工作量**：M  
**置信度**：CONFIRMED（未鉴权安装链已确认；浏览器利用受其本地网络访问策略影响）

**范围说明**：当前打包版本已禁止选择 Node 后端，本项影响显式启动它的开发环境。

### S-02｜High｜CWE-863：preload 允许页面借用全权 daemon 令牌

**位置**：`apps/desktop/src/main/preload.ts:37`；`apps/desktop/src/main/ipc/index.ts:88`、`:119`、`:316`；`apps/desktop/src/main/main.ts:164`

**攻击场景**：攻击者先获得主窗口页面的 JavaScript 执行能力，例如渲染层注入或开发页面被控制。随后调用 `window.agent24.backendProxy`，向审批、模块管理、记忆设置或 shutdown 等接口发送请求。主进程自动附加 legacy 全权 bearer，页面无需读取令牌即可执行这些操作。

**证据**：

- preload 暴露通用 `backendProxy(req)`。
- 请求校验只限制 HTTP 方法及绝对路径形式，没有业务操作白名单。
- IPC handler 忽略 `_event`，不验证调用 frame 或页面来源。
- 转发时自动加入 `Authorization: Bearer <endpoint.token>`。
- 主窗口启用 `contextIsolation`、关闭 `nodeIntegration`，但 `sandbox:false`；也未像 Creative 窗口一样设置导航限制。

**建议修补**：按业务操作暴露 IPC；主进程同时校验发送者、主 frame 和固定来源。删除任意路径转发，提升级操作绑定独立确认。补齐主窗口导航限制并打包 preload 后开启 sandbox。

**工作量**：L  
**置信度**：CONFIRMED

**限制**：本次未确认独立的渲染层注入入口；确认的是取得页面执行能力后的全权代理链。

### S-03｜High｜CWE-829／授权顺序错误：模块在“待同意、禁用”之前已执行

**位置**：`packages/node-daemon/src/module-installer.ts:57`、`:100`、`:111`；`packages/node-daemon/src/server.ts:174`、`:179`、`:186`

**攻击场景**：用户合法安装第三方模块，但尚未审阅权限或启用模块。恶意发布者将副作用放在 npm 生命周期脚本或入口文件顶层。安装过程先执行这些代码，再校验 manifest、记录禁用状态。即使最终返回“无有效模块，已回滚”，代码也已执行。

**证据**：

- `npm install` 未使用 `--ignore-scripts`。
- `require(modulePath)` 早于 `validateManifest(mod.manifest)`。
- 服务端先调用 `loadInstalledModule`，之后才执行 `setEnabled(..., false)`。
- 注释承诺启用前不发生副作用，但实际仅约束后续注册的服务启动。

**建议修补**：安装阶段禁用脚本；从静态文件读取并校验 manifest，禁止通过 `require` 获取清单。取得明确执行授权前不得加载模块代码。若要承诺权限隔离，执行应放入受限进程。

**工作量**：M  
**置信度**：CONFIRMED

**范围说明**：独立于 S-01；即使安装请求已鉴权，合法安装流程仍存在此问题。

### S-04｜Medium｜CWE-732：数据目录的权限加固晚于敏感文件打开

**位置**：`rust/crates/agent24-protocol/src/state_file.rs:209`、`:170`；`rust/apps/agent24d/src/server.rs:1590`、`:1606`；`rust/apps/agent24-cli/src/service.rs:232`、`:244`

**攻击场景**：首次启动使用可被其他用户遍历的父目录及常见 `umask 022`。其他 UID 在启动期间打开可读数据库或服务日志，并保留文件描述符。daemon 稍后将目录改为 0700，也不能撤销已打开的句柄；日志句柄还可能读到随后输出的全权 ready token。

**证据**：

- 单例锁入口使用普通 `create_dir_all(dir)`，没有先建立或验证私有目录。
- 数据库与记忆库在发布发现文件之前打开。
- 0700 设置发生在后面的 `state_file::write`。
- service 安装先普通创建 `logs/`，未预创建并限制 stdout 日志权限。
- legacy ready 行包含 token，并被 LaunchAgent 的 `StandardOutPath` 持久记录。

**建议修补**：启动最前端建立并验证 owner-only 数据根目录，成功后才能创建锁、日志或数据库；日志预创建为 0600。服务模式不要把 bearer 写入常规 stdout 日志。

**工作量**：M  
**置信度**：PLAUSIBLE（顺序缺口已确认；跨 UID 利用取决于实际父目录、umask 和日志文件权限，未运行验证）

### S-05｜Medium｜CWE-613：ProductHost 实际永不过期且始终全权

**位置**：`rust/apps/agent24d/src/server.rs:1570`；`rust/apps/agent24d/src/capabilities/mint.rs:65`；`rust/apps/agent24d/src/capabilities/validate.rs:118`

**攻击场景**：宿主令牌因宿主缺陷、误记录或其他泄露被复制，且原宿主与 daemon 继续运行。取得副本的进程可持续调用审批、授权管理、通信写入和 shutdown 等接口，不会在一小时后失效，也不需要证明用户在场。

**证据**：

- 启动使用 `Duration::from_secs(u64::MAX)`。
- 到期时间使用饱和加法，实际得到 `u64::MAX`。
- Operation 和资源约束只对 `CreativeRuntime` 执行，ProductHost 不受限制。
- 路由的 Everyday／Elevated 当前仅为元数据，尚未形成提升校验。
- 当前父管道 EOF 会停止 daemon，因此风险期限是本代 daemon 的存活期。

**建议修补**：落实 CORE-2 §9 Q4 的一小时 TTL；仅通过原父管道续期，原子撤销旧令牌；按设计交付独立的提升授权。

**工作量**：L  
**置信度**：CONFIRMED

**边界说明**：这是凭据泄露后的持续权限风险，不是 Creative 白名单绕过，也不以恶意同 UID 任意读取宿主内存作为漏洞前提。

## 已核实但未另列漏洞

- **HEAD／OPTIONS／404**：capability 模式先验证活跃 bearer；HEAD 使用 GET 映射。未发现匿名绕过。
- **Creative**：实际路由仍只允许 `GET /api/v1/models`，不能因新增 Operation 映射进入会话、run 或事件接口。
- **WS**：`rust/apps/agent24d/src/events.rs:81` 起仅保留 socket 与订阅，不复验令牌，也不按资源过滤。但当前 Creative 无法进入，ProductHost 又实际永不过期；因此未将其表述为已确认的低权窃听漏洞。有限期令牌开放事件流前必须补齐。
- **REST Host／Origin**：Rust REST 缺少校验，但业务接口仍要求 bearer；未据此声称匿名 DNS rebinding 或 CSRF 可执行业务。
- **本机模块**：attach 父目录检查属主和 0700，连接检查 UID；回调有帧长、并发和超时限制。未把同 UID 无隔离、路径检查的既知竞态直接升级为漏洞。
- **密钥及供应链**：COMM 口令使用清零容器并经 stdin 交付；未确认口令或 Nostr 私钥进入 argv／普通日志。本次没有依据离线版本号、`unsafe` 或签名文字推断依赖漏洞。

## 建议修补顺序 Top 5

1. **S-01**：关闭 Node 未鉴权安装执行入口。
2. **S-02**：收窄桌面 IPC，阻断页面借用全权 bearer。
3. **S-03**：移除同意前的安装脚本与模块入口执行。
4. **S-04**：在所有敏感文件操作前建立私有目录，并停止记录 ready bearer。
5. **S-05**：落实 ProductHost 有限 TTL、原管道续期及提升授权。