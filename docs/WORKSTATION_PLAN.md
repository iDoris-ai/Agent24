# 个人本地 AI 工作站规划

> 状态：规划中（未开始实现）
> 核心引擎：oMLX（Apple Silicon 优化）+ Ollama（备选/Windows）
> 目标机器：64GB Mac（M2 Max/M3 Max/M4 Max）

---

## oMLX 可编程 API 速查

> 调研来源：jundot/omlx 源码（2026-05-09）

### 标准 OpenAI 兼容层（`localhost:8000/v1`）
| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/v1/models` | 列出所有模型（含加载状态） |
| POST | `/v1/chat/completions` | 对话推理（流式） |
| POST | `/v1/completions` | 文本补全 |
| POST | `/v1/messages` | Anthropic Messages 兼容 |
| POST | `/v1/embeddings` | 文本嵌入 |
| POST | `/v1/rerank` | 文档重排序 |
| GET | `/health` | 健康检查 |

### 管理 API（`localhost:8000/admin/api`，需 API Key）
| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/admin/api/models` | 列出模型 + 加载状态 + 设置 |
| POST | `/admin/api/models/{id}/load` | **加载指定模型到内存**（Bearer Token 即可）|
| POST | `/admin/api/models/{id}/unload` | 卸载模型 |
| PUT | `/admin/api/models/{id}/settings` | 修改设置（pin/default/TTL/别名等）|
| POST | `/admin/api/hf/download` | **从 HuggingFace 下载模型** |
| GET | `/admin/api/hf/tasks` | 查询下载任务进度 |
| POST | `/admin/api/hf/cancel/{task_id}` | 取消下载 |
| GET | `/admin/api/hf/search` | 搜索 HF 模型 |
| GET | `/admin/api/stats` | 服务器统计 |

### Ollama 对比
| 能力 | oMLX | Ollama |
|------|------|--------|
| 列出本地模型 | `GET /admin/api/models` | `GET /api/tags` |
| 查看运行中模型 | 同上（含 engine 状态） | `GET /api/ps` |
| 下载模型 | `POST /admin/api/hf/download` | `POST /api/pull`（流式进度）|
| 加载模型 | `POST /admin/api/models/{id}/load` | 首次调用时自动加载 |
| 卸载模型 | `POST /admin/api/models/{id}/unload` | `keep_alive: 0` |
| API 认证 | Bearer Token / 无（可配置）| 无（默认）|

**关键结论**：
- oMLX 可以**完全编程控制**模型下载 + 加载，只需 API Key
- Ollama 更简单：下载即用，首次 `/api/chat` 自动加载
- 插件可以通过 LLM Gateway 声明所需模型，Gateway 自动调对应运行时的管理 API

---

## 插件 + 模型管理设计（待 ADR-026 正式化）

```
插件声明所需模型
    ↓
LLM Gateway 检查 /v1/models 是否已加载
    ├── 已加载 → 直接调用
    ├── 已下载未加载 → POST /admin/api/models/{id}/load
    └── 未下载 → POST /admin/api/hf/download → 轮询进度 → load → 调用
```

Ollama 路径更简单（下载+加载合一）：
```
POST /api/pull → 流式返回进度 → 完成后直接 /api/chat 调用
```

---

## 推荐模型清单（64GB Mac）

> 按用途分组，标注内存占用，固定加载 vs 按需加载

### 固定加载（常驻内存，~25GB）
| 用途 | 模型 | 量化 | 内存 |
|------|------|------|------|
| 日常对话/分析/写作 | `mlx-community/Qwen3-30B-A3B-4bit` | 4bit | ~18GB |
| 多语言嵌入（RAG） | `mlx-community/bge-m3` | — | ~1.5GB |
| ASR | `mlx-community/whisper-large-v3-mlx` | — | ~3GB |
| 重排序 | `mlx-community/ModernBERT-reranker` | — | ~0.5GB |

### 按需热切换（视任务选用）
| 用途 | 模型 | 量化 | 内存 |
|------|------|------|------|
| 视觉理解/图片分析 | `mlx-community/Qwen2.5-VL-7B-Instruct-4bit` | 4bit | ~5GB |
| OCR | `mlx-community/DeepSeek-OCR` | — | ~3GB |
| 复杂推理 | `mlx-community/Qwen3.5-32B-4bit` | 4bit | ~20GB |
| 代码 | `mlx-community/Qwen3-Coder-32B-8bit` | 8bit | ~35GB |

### 外部工具（非 oMLX 管理）
| 用途 | 工具 | 运行方式 |
|------|------|---------|
| 文生图/图生图 | ComfyUI + FLUX.1-schnell-MLX | 独立进程，HTTP API |
| 视频生成 | ComfyUI + AnimateDiff | 独立进程，HTTP API |
| TTS | Kokoro-82M (ONNX) | Python 子进程 |
| 语音克隆 | CosyVoice2 | Python 子进程 |

---

## 工作站能力 TODO List

> 每项标注：能力名 / 核心技术栈 / 依赖模型 / 优先级 / 状态

### 第一批：信息处理核心（最高优先，无额外模型依赖）

- [ ] **LLM Gateway 多运行时支持**
  - oMLX adapter（当前 M2 骨架只有 Ollama）
  - Ollama adapter
  - OpenAI-compatible remote adapter（Claude/OpenAI/DeepSeek）
  - 运行时自动检测（先试 oMLX:8000，再试 Ollama:11434）
  - 设置页 UI：下拉切换 + 地址/端口/API Key

- [ ] **模型管理 API 封装**（`src/backend/model-manager.ts` → M3 Python）
  - `ensureModelLoaded(modelId)` — 检查 → 下载 → 加载，返回 Promise
  - oMLX 路径：`/admin/api/hf/download` + `/admin/api/models/{id}/load`
  - Ollama 路径：`/api/pull`（流式进度）
  - 进度回调：通过 WebSocket 推到前端进度条

- [ ] **RAG 归档查询**
  - 向量化：oMLX BGE-M3 嵌入接口
  - 向量库：ChromaDB（Python，本地文件）
  - 重排序：oMLX ModernBERT-reranker
  - 查询 API：`POST /api/v1/rag/query`
  - 入库 API：`POST /api/v1/rag/ingest`（支持 txt/md/pdf/html）

### 第二批：信息搜集

- [ ] **网页抓取 + 内容提炼**
  - Crawl4AI（Python，异步，专为 LLM 优化）
  - Playwright 备选（复杂登录页）
  - 流程：URL → 抓取正文 → oMLX 摘要 → 存 SQLite + RAG
  - API：`POST /api/v1/crawl`，`POST /api/v1/crawl/batch`

- [ ] **RSS/订阅监控**
  - feedparser 定时拉取
  - 新条目 → LLM 评分（相关性 0-10）→ 高分推送通知
  - API：`POST /api/v1/feeds`（添加订阅），`GET /api/v1/feeds/digest`

### 第三批：媒体能力

- [ ] **ASR 语音识别**
  - 引擎：`mlx-whisper`（Apple Silicon 原生）
  - API：`POST /api/v1/asr`（文件上传 → 文字）
  - 场景：视频字幕生成、会议记录、语音输入
  - 模型：`mlx-community/whisper-large-v3-mlx`

- [ ] **TTS 文字转语音**
  - 引擎：Kokoro-82M（ONNX，轻量，CPU 可跑）
  - API：`POST /api/v1/tts`（text + voice_id → 音频文件）
  - 场景：播客生成、有声内容

- [ ] **语音克隆**
  - 引擎：CosyVoice2（3秒参考音频克隆）
  - API：`POST /api/v1/voice-clone`（参考音频 + 文本 → 克隆语音）
  - 场景：视频配音保持一致音色

- [ ] **文生图**
  - 引擎：ComfyUI（独立进程）+ FLUX.1-schnell-MLX
  - API：`POST /api/v1/image/generate`（prompt → 图片）
  - 与工作流引擎集成：图生成作为工作流 Step

- [ ] **图生图**
  - 引擎：ComfyUI + img2img workflow
  - API：`POST /api/v1/image/transform`（图片 + prompt → 图片）

- [ ] **视频生成**
  - 引擎：ComfyUI + AnimateDiff 或 CogVideoX-5B
  - API：`POST /api/v1/video/generate`（prompt/图片 → 视频）
  - 内存约束（ADR-025）：与 LLM 串行，不并发

- [ ] **格式转换**
  - 文档：Pandoc（md/docx/pdf/html 互转）
  - 音视频：ffmpeg-python
  - API：`POST /api/v1/convert`（文件 + target_format → 文件）

### 第四批：翻译

- [ ] **翻译能力模块**
  - 引擎：oMLX Qwen3-30B（多语言，质量远超 Gemma-2B）
  - 声明所需模型：`mlx-community/Qwen3-30B-A3B-4bit`（常驻，复用）
  - API：`POST /api/v1/translate`（text + source_lang + target_lang → text）
  - 批量翻译：`POST /api/v1/translate/batch`
  - 参考：gemma-chrome-translate 的 prompt 设计（vendor/gemma-chrome-translate）

### 第五批：工作流 + 发布

- [ ] **工作流引擎**（ADR-024：asyncio.Queue + Step）
  - Step 接口：`async def run(ctx: WorkflowCtx) -> StepResult`
  - 内置模板：短视频生成发布、舆情抓取分析、内容翻译归档
  - API：`POST /api/v1/workflow/run`，`GET /api/v1/task/{id}`
  - WebSocket：`/ws/task/{id}` 实时进度

- [ ] **媒体平台发布**
  - Playwright 自动化（参考 vendor/xiaoheishu 现有实现）
  - 平台：小红书（已有参考）/ 公众号 / 微博
  - API：`POST /api/v1/publish`（platform + content + media）

- [ ] **媒体平台监测**
  - 定时抓取评论/数据
  - oMLX 分析情感（正/负/中性）
  - 异常告警推送（通过 Nostr/微信）

### 第六批：Onboarding（ADR-021）

- [ ] **硬件检测**（`src/main/hardware-detect.ts`）
  - RAM：`os.totalmem()`
  - GPU：systeminformation 包（Metal / CUDA 识别）
  - 基于配置推荐模型组合

- [ ] **oMLX 安装管理**（`src/main/omlx-manager.ts`）
  - 检测：`GET localhost:8000/health`
  - 未安装：引导下载 oMLX .dmg（GitHub releases）
  - Mac 用户：静默安装 + 配置 `~/.omlx/settings.json`

- [ ] **模型首次下载向导**
  - 基于硬件检测结果推荐模型套装
  - 调用 oMLX `POST /admin/api/hf/download` 批量下载
  - 进度条展示（WebSocket 轮询下载任务）
  - 完成后固定常用模型（`is_pinned: true`）

- [ ] **5步 Onboarding Wizard UI**（React）
  - Step 1: 欢迎
  - Step 2: 环境检测（自动）
  - Step 3: 模型推荐（可手动调整）
  - Step 4: 下载进度
  - Step 5: 就绪 → 进主界面

---

## 插件标准接口草案（ADR-026 待正式化）

```python
class CapabilityPlugin:
    id: str                           # "translate", "asr", "tts"...
    name: str                         # 显示名
    version: str
    required_models: list[str]        # 插件声明所需模型 ID
    required_tools: list[str]         # 外部依赖 ["comfyui", "ffmpeg"]

    async def register(self, app: FastAPI, gateway: LLMGateway) -> None: ...
    async def health(self) -> dict: ...  # {"status": "ok"|"degraded", "reason": "..."}
```

**引入方式**：
1. **官方内置插件**：`src/backend/capabilities/` 目录，随 app 打包
2. **pip 安装**：`pip install auraaihq-plugin-translate`，自动发现（entry_points 机制）
3. **本地开发插件**：放入 `~/.agent24/plugins/` 目录，动态扫描加载

**gemma-chrome-translate 的定位**：
- 作为 vendor submodule 保留，学习其 prompt 工程和 UI 设计
- 我们实现 `TranslationPlugin`（能力更强，用 oMLX 30B 模型），不依赖 Chrome API
- 如果用户需要浏览器内翻译，仍可安装原版 Chrome 扩展（两者不冲突）

---

## 技术约束备忘

- 内存限制：oMLX `--max-process-memory auto`（= RAM - 8GB = 56GB）
- LLM 串行：同时最多 1 个推理（ADR-025）
- ComfyUI 与 LLM 互斥：不并发（各占 ~8-12GB）
- 工作流 Step 可并发，仅 LLM/图像 Step 串行
- 安全：仅 localhost，外网访问走 Tailscale（ADR 待补）
