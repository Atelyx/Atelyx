# 样式与容器契约

插件的视图与槽位贡献都落在**宿主容器**里，样式一律靠 **CSS 变量**跟随主题。三条规则：
**不自绘外壳**、**token 只读**、**遵守槽位尺寸**。照做即可「一进来就好看」，不需要为两套主题
（浅 / 深）各写一份样式，也不需要知道当前是哪套。

## 1. 容器注入：不自绘外壳

`ctx.slots.registerView` 注册的组件挂在宿主面板壳内——**标签头（标签组 / 状态指示 / ≡ 菜单）由宿主渲染**，
你只负责内容区。

- 内容区**自己撑满**：根元素写 `style={{ height: "100%", display: "flex", flexDirection: "column" }}`。
- **不要写死宽高**：面板可被用户 resize、可撕裂成独立窗口，固定尺寸会坏掉。
- **不要自绘面板边框 / 标题栏 / 面板底色**：这些已由宿主给；也不要给自己套圆角 + 阴影当卡片壳
  （面板不是浮层）。
- 空态别自造一套语言：用 `empty/<viewKind>` 槽贡献，或复用宿主 `PanelPlaceholder` 的形态（图标 + 标题 + 说明）。

## 2. token 只读：内联 `style` + CSS 变量

样式一律写内联 `style`，值引用 CSS 变量：

```tsx
<div style={{
  background: "var(--bg-secondary)",
  color: "var(--text-primary)",
  border: "1px solid var(--border)",
  borderRadius: "var(--radius-sm)",
}}>
  面板工具条
</div>
```

**不要用 Tailwind class 写样式。** 宿主的 Tailwind 只编译应用自身源码里出现过的类名，插件源码不在
扫描范围内——`className="bg-slate-800"` 或 `className="bg-[var(--bg-secondary)]"` 只有在**恰好**与宿主
某个类名相同时才生成对应 CSS，其余情况**静默不生效**，且没有任何报错。

**不要硬编码颜色，也不要判主题**：浅 / 深两套由宿主切换变量取值，写死色值在另一套主题下必然错。

### 可用变量（只读，只增不改）

| 组 | 变量 |
| --- | --- |
| 背景层级 | `--bg-sunken` `--bg-primary` `--bg-secondary` `--bg-tertiary` `--bg-card` `--bg-overlay` `--surface` `--scrim` |
| 文本 | `--text-primary` `--text-secondary` `--text-muted` |
| 边与交互态 | `--border-subtle` `--border` `--border-strong` `--hover` |
| 表单与滚动条 | `--input-bg` `--input-border` `--input-placeholder` `--scrollbar-thumb` `--scrollbar-thumb-hover` |
| 强调色（品牌金） | `--accent` `--accent-hover` `--accent-fg` `--accent-soft` `--focus-ring` |
| 状态语义 | `--success` `--warning` `--danger` `--danger-fill` `--info` |
| 圆角与投影 | `--radius-xs` `--radius-sm` `--radius-bubble` `--radius-md` `--radius-lg` `--shadow-pop` |
| 字体 | `--font-sans` `--font-display` `--font-mono` |
| 代码与表格 | `--code-bg` `--table-border` `--table-header-bg` `--highlight-bg` `--link-internal` |

**层级用法约定**（相邻档差很小，分层靠「色阶 + 1px 边」而不是强对比）：

- 面板内容区 = `--bg-primary`；工具条 / 面板头 = `--bg-secondary`；面板内嵌卡片 = `--bg-tertiary`。
- 自己弹出的浮层（菜单 / 弹窗）= `--bg-overlay` + `--shadow-pop`（投影只给浮起的元素，面板内部别用）。
- 需要「下沉」的面（输入框、代码块）= `--bg-sunken`。

**强调色与前景**：`--accent` 作背景时，其上的文字恒用 `--accent-fg`（深 / 浅主题下取值不同，别写白字）；
文字用强调色时直接 `color: var(--accent)`。焦点环用 `var(--focus-ring)`，别自绘别的颜色。

**半透明叠加**用 `color-mix`，别写 `rgba` 死值：

```tsx
style={{ background: "color-mix(in srgb, var(--text-primary) 12%, transparent)" }}
```

**危险 / 警告 / 成功**一律用语义变量（`--danger` / `--warning` / `--success`），不要用框架默认的红黄绿：

```tsx
style={{ color: "var(--danger)", background: "color-mix(in srgb, var(--danger) 12%, transparent)" }}
```

**间距**用固定 px（4 的倍数：4 / 6 / 8 / 12 / 16 / 20 / 24 / 32），宿主不提供间距变量。

## 3. 槽位尺寸契约

每个槽位在宿主布局里占一个固定位置。**宿主不做截断或折叠**——超限会挤坏整条工具条，
所以尺寸由插件自行遵守：

| 槽位 | 排列 | 尺寸约定 |
| --- | --- | --- |
| `toolbar/*`（左 / 右） | 行内横排 | 控件高 24–28，图标 12–16；单个贡献**不换行、不超一行**；放不下就用「图标 + `title` 提示」，别塞长文本 |
| `panelhead/status` | 行内 | 只放状态（文字 / 小徽标），不放大按钮；宽度尽量 < 160px |
| `panelhead/actions` | 行内 | 图标按钮 24×24，建议最多 2–3 个 |
| `statusbar/canvas` | 行内 | 纯文本 / 小徽标，高度跟随状态栏（不要撑高） |
| `gutter/note` | 纵向 | 图标 12–16，不要撑宽 gutter |
| `titlebar/right` | 行内 | 图标按钮，贴合标题栏高度 |
| `settings/*` | 区块 | 纵向排列、**占满容器宽度**；区块自带标题与下间距（16 / 24）；不要自带外边距撑破设置页 |
| `contextmenu/*` | 菜单项 | 只提供 `{ label, onClick }`，由宿主菜单渲染，不要自绘 |

## 4. 状态与无障碍

- **状态不得只靠颜色**：色 + 图标或文字，至少两重编码。
- 键盘可达的元素用 `var(--focus-ring)` 作焦点样式。
- 正文对比度 ≥ 4.5:1，次要文本 ≥ 3:1。