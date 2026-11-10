# 微信桥（开发工具）

本包是开发工具，不进产品。入站执行将经 ADR-041a 准入重做；当前桥内名单仅为开发便利，不代表产品准入。

启动入口默认拒绝运行，只有显式设置 `A24_DEV_TOOL=1` 才允许继续发现 daemon、登录微信。采用显式开发标记而非猜测 Electron 或数据目录，产品启动流程不得设置此标记；它不是抵御同 UID 操作者的安全边界。

```sh
# 先启动开发用 daemon，再配置允许的微信用户
A24_DEV_TOOL=1 A24_WECHAT_ALLOWED_UIDS='<开发用户 ID>' pnpm --filter @agent24/wechat-bridge bridge
```

首次运行打印二维码。连接配置使用 `A24_BASE_URL` / `A24_TOKEN` 或开发 daemon 的状态文件。桌面打包配置不得引用 `packages/*-bridge`（由桌面打包测试检查）。参见 [ADR-044](../../docs/adr/ADR-044-communication.md)。
