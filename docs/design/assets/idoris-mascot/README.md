# iDoris 像素小人

ODX-6 英雄位的素材。从 `apps/desktop/src/renderer/assets/logo-welcome.png` 降采样得来，不是手绘草稿。

## 重新生成

在仓库根目录：

```bash
python3 tools/mascot/build_mascot.py
```

依赖 Pillow。脚本会核对调色板里的每个颜色都在原图里出现过。`--compare` 把长边 40 / 48 / 64 的对照图写到 `/tmp/mascot-compare/`，不改这里的文件。

## 网格

人物包围盒大约 123×177，是竖的。方的 48×48 会把她压扁，所以按长边比较，并保持宽高比：

| 长边 | 网格 | 结果 |
|---|---|---|
| 40 | 28×40 | 眼睛糊成两个点 |
| 48 | 33×48 | 能认出人，眼睛和拉链发软 |
| 64 | 44×64 | 马尾、眼睛、兜帽、拉链都还在 |

选定长边 64。画布是 **48×66**：44×64 的人物放在 `(2, 1)`，四周留出 1px 描边，头顶留状态标记。显示宽 72px 时，高度约 99px。

采样是每个格子取不透明像素的众数，不用插值。半透明的白边当成背景抠掉。不透明的白兜帽、内衬留着。

胸口的「iDORIS」在这个尺寸下是一堆碎点，收成一块 4×2 的白标签，左边一颗橙点当作 i。右袖上的浅色块收成 4×4 的空心方框，位置还在原来的袖子上。

## 调色板

8 色，都是原图里的真实像素。`palette.json` 里有每色占了多少格。

| 名字 | hex | 用在哪 |
|---|---|---|
| black | `#000000` | 瞳孔、眉毛、最深的缝 |
| ink | `#252525` | 头发和羽绒服（原图里最多的深色） |
| silver | `#A0A0A0` | 袖标金属边、帽檐灰 |
| white | `#FEFEFE` | 兜帽、内衬、眼白、胸口标签 |
| skin | `#F9D7B9` | 皮肤亮部 |
| skin_deep | `#F3C59E` | 皮肤暗部 |
| blush | `#E79B75` | 腮红和嘴 |
| orange | `#F77315` | 拉链、发绳、袖口、漂浮的橙点、状态标记 |

深色轮廓外侧加了 1px 半透明白（`#FFFFFF`，不透明度 0.7）。浅底上几乎看不见。深底 `#111318` 上，黑发和羽绒服靠这圈描边分开。`preview-light.png` 和 `preview-dark.png` 是四种状态横排，格子放大 4 倍。

## 四种状态

覆盖层是脚本里的 `{x, y, color}` 列表，改的格子很少。待机就是基础图。

| 文件 | SVG 组 | 什么时候用 | 画面 |
|---|---|---|---|
| `idle.svg` | `#idoris-state-idle` | 默认 | 基础图 |
| `thinking.svg` | `#idoris-state-thinking` | 有运行在进行 | 眼睛半闭，头顶三个橙点 |
| `success.svg` | `#idoris-state-success` | 这一轮产物写完 | 眼睛收成笑眼，左上角一颗橙星 |
| `error.svg` | `#idoris-state-error` | 运行失败 | 眉头下压，嘴改平，左侧一个橙色「!」 |

每个文件里还有一组 `#idoris-outline`。同一时刻只显示一个状态组。文件里没有动画；系统打开「减少动态」时停的是组件里的位移，不是这张图。

## 接到 ODX-6 时放哪

槽位对应 od-plan worktree 里 `docs/design/OPEN-DESIGN-IDORIS-EDITION.md` 的「小人设计规格」。本仓库没有这份计划。

英雄槽替换 fork 里的 `IDorisDesignLogo`（`c49ba6a301` 那个圆头小画家）。挂在新 Home 标题旁边，宽约 72px，`shape-rendering: crispEdges`。旁边的「iDoris Design」继续用 HTML，不画进 SVG。状态用上表的组 id，由组件的 `data-state` 决定显示哪一组。第一版可以只接待机，另外三组先留在文件里。

浮动宠物不换。那一槽是 `PetOverlay` 加 `codexAtlas.ts` 的 8×9 图集（1536×1872）。不要把这张 48×66 的 SVG 拉去当宠物。
