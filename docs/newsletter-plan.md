# Newsletter 系统方案

文档日期：2026-05-13（更新：2026-05-15）  
归属：Mycelium Blog (blog.mushroom.cv) × Agent24

---

## 职责边界

| 角色 | 仓库 | 职责 |
|------|------|------|
| **运营方** | Agent24（本仓库） | 部署运行 Listmonk、管理订阅者、配置 SMTP、定期发送 newsletter |
| **内容提供方** | mycelium/blog | 产出博客内容，通过 RSS 供 Listmonk 拉取；在博客页面嵌入订阅入口 |

Mycelium Blog **不负责** Listmonk 的部署和运维，只作为内容来源和订阅流量入口。

---

## 完整数据流

```
博客页面 (Cloudflare Pages 静态)
    │
    │  用户填邮箱 → 点击"订阅"
    │  POST → Listmonk 公开订阅 API
    ▼
Listmonk（Agent24 机器，Docker 运行）
    ├── 自带 PostgreSQL —— 订阅者数据存这里（不是 Cloudflare）
    ├── 自动发确认邮件 → 用户点确认 → 状态变 confirmed
    └── SMTP（Resend）→ 批量发 newsletter
    
内容发送流程（脚本 + 定时触发）：
    博客 RSS (/rss.xml)
        → 脚本格式化为 HTML
        → 调 Listmonk Campaign API 创建期刊
        → Listmonk 批量发给所有 confirmed 订阅者
```

**关键说明**：订阅者数据存在 Listmonk 自带的 PostgreSQL（在 Agent24 运营的机器上），不经过 Cloudflare。博客前端只需要一个表单，POST 到 Listmonk 的公开 API 即可。

---

## 工具选型：Listmonk

调研了 Listmonk、Keila、Plunk、Ghost、phpList、SendPortal 六个方案，选 Listmonk。

| 工具 | 内存 | GitHub ⭐ | 活跃度 | 结论 |
|------|------|---------|-------|------|
| **Listmonk** | 50–150 MB | 18,936 | ⭐⭐⭐⭐⭐ v6.1.0 @ 2026-03 | ✅ 选用 |
| Keila | ~1 GB | ~2,000 | ⭐⭐⭐ | 内存高，社区小 |
| Plunk | ~512 MB | 4,827 | ⭐⭐⭐ | 社区不如 Listmonk |
| Ghost | ~1 GB+ | 高 | ⭐⭐⭐⭐ | 太重，完整 CMS |

选用理由：Go 单二进制、内存极低、功能完整、社区最大、支持任意 SMTP。

完整调研见：`mycelium/blog/newsletter/RESEARCH.md`

---

## 部署资源需求

| 组件 | 内存 |
|------|------|
| Listmonk 容器 | 50–150 MB |
| PostgreSQL 容器 | 150–300 MB |
| **合计** | **~300–450 MB** |

最低建议：512 MB 可用内存。

---

## Listmonk 仓库

- Fork：https://github.com/jhfnetboy/listmonk
- 原仓库：https://github.com/knadh/listmonk

---

## 实施步骤

### Agent24 负责（运营方）

- [ ] 确认部署机器内存规格
- [ ] 编写 `docker-compose.yml`（Listmonk + PostgreSQL）
- [ ] 配置 Resend SMTP，验证 `blog.mushroom.cv` 域名
- [ ] 配置订阅列表，获取 list UUID（供博客表单使用）
- [ ] 编写内容脚本：RSS → HTML → Listmonk Campaign API
- [ ] 配置定时触发（GitHub Actions 或 cron），每周发一期
- [ ] 对外暴露 Listmonk 公开订阅 API 地址

### mycelium/blog 负责（内容方）

- [ ] 收到 Listmonk API 地址和 list UUID 后，在博客页面嵌入订阅表单
- [ ] 保持 `/rss.xml` 准确更新（newsletter 内容来源）

---

## 相关链接

- [Listmonk 官网](https://listmonk.app)
- [Listmonk 文档](https://listmonk.app/docs/)
- [Listmonk API 文档](https://listmonk.app/docs/apis/apis/)
- [Listmonk Docker Hub](https://hub.docker.com/r/listmonk/listmonk)
- [Resend 文档](https://resend.com/docs)
- [博客 RSS](https://blog.mushroom.cv/rss.xml)
