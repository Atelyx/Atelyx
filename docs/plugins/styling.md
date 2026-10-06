# 样式与容器契约

插件的视图与槽位贡献都落在**宿主容器**里，样式一律靠 **CSS 变量**跟随主题。三条规则：
**不自绘外壳**、**token 只读**、**遵守槽位尺寸**。照做即可「一进来就好看」，不需要为两套主题
（浅 / 深）各写一份样式，也不需要知道当前是哪套。

## 1. 容器注入：不自绘外壳

`ctx.slots.registerView` 注册的组件挂在宿主面板壳内——**标签头（标签组 / 状态指示 / ≡ 菜单）由宿主渲染**，
你只负责内容区。**唯一例外是外壳接管（`ctx.slots.registerShell`）**：接管者自绘全部外壳
（标题栏 / 导航 / 窗口控制），但本节以下的 token 规则不变。

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
| 字阶 | `--fs-display` `--fs-h1` `--fs-h2` `--fs-body` `--fs-ui` `--fs-caption` `--fs-micro` + 各自配对的 `--lh-*` |
| 动效 | `--dur-fast` `--dur-base` `--dur-slow` `--ease` |
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

**间距**用固定 px，宿主不提供间距变量。常规档是 4 的倍数（4 / 6 / 8 / 12 / 16 / 20 / 24 / 32）；
工具条、属性行、徽标这类密集处允许 2 / 3。**不要用 1px 以下的间距**（发丝边框除外），
也不要出现 5 / 7 / 9 这类既非 4 倍数、又在密集档之外的中间值。

### 字阶：字号与行高成对取

字号取 `--fs-*`，行高取**同名配套**的 `--lh-*`（两组一一对应，不要交叉搭配）：

| 档 | 变量 | 用在哪 |
| --- | --- | --- |
| display | `--fs-display` / `--lh-display` | 视图内主标题（空态标题、大标题） |
| h1 | `--fs-h1` / `--lh-h1` | 区块大标题 |
| h2 | `--fs-h2` / `--lh-h2` | 区块标题 |
| body | `--fs-body` / `--lh-body` | 正文、消息、笔记内容 |
| **ui** | **`--fs-ui` / `--lh-ui`** | **界面默认档**：面板标签、菜单、按钮、列表 |
| caption | `--fs-caption` / `--lh-caption` | 次要说明、状态栏 |
| micro | `--fs-micro` / `--lh-micro` | 徽标、计数、快捷键提示 |

```tsx
style={{ fontSize: "var(--fs-ui)", lineHeight: "var(--lh-ui)" }}
```

**不要写 px 字号**：字阶是 rem 基准，会随应用级「字体大小」设置整体缩放；写死 px 的文字不跟着缩放，
会在同一块面板里与已缩放文字出现字号断层。**主次靠档位与字重区分，不靠同号变色。**

长文正文块套 `--note-line-width` 收宽（它是用户可在「设置 → 编辑器」里调的量，读这个变量即可跟随，
不要写死自己的行宽）。

### 动效：只给状态变化

三档时长 + 一条统一曲线，仅用于**状态变化**（浮现、展开、高亮），不做装饰性动画：

```tsx
style={{ transition: "background-color var(--dur-fast) var(--ease)" }}
```

| 档 | 值 | 用在哪 |
| --- | --- | --- |
| `--dur-fast` | 120ms | 悬停反馈、聚焦反馈 |
| `--dur-base` | 180ms | 默认档：展开、淡入、底色过渡 |
| `--dur-slow` | 260ms | 大范围变化（面板内容整体替换） |

曲线统一 `var(--ease)`，不要自己写 `cubic-bezier`。

**「进行中」不进这三档**：循环扫光、加载条、思考指示、闪烁光标表达的是「还没结束」而非状态切换，
节奏由辨识度决定，按档位改会变成另一种信号。加载指示见 §5。

## 3. 控件尺寸档

宿主的按钮 / 图标按钮用的是固定档位，插件贡献的控件按同一批尺寸走，整条工具条上的图标
才会对齐。**方形图标按钮**的边长（含内边距的整边长）：

| 档 | 边长 | 用在哪 |
| --- | --- | --- |
| 2xs | 16 | 键值 chip 上的删除、密集属性行内联 |
| xs | 20 | 属性 chip、列表行内联操作 |
| **sm** | **24** | **工具条、面板头、次要动作（默认档）** |
| md | 28 | 面板头主按钮、空态行动 |
| lg | 32 | 视图标题栏 |
| touch | 44 | 移动端（≥44 触控目标） |

**带文字的按钮**取同一批档位的控件高（16 / 20 / 24 / 28 / 32 / 44），圆角随档走：
小档 `--radius-xs`（4）、中档 `--radius-sm`（6）、大档 `--radius-md`（10）。

**图标本身**比按钮边长小：12–16 是常规，密集处用 11–13。

## 4. 槽位尺寸契约

每个槽位在宿主布局里占一个固定位置。**宿主不做截断或折叠**——超限会挤坏整条工具条，
所以尺寸由插件自行遵守：

| 槽位 | 排列 | 尺寸约定 |
| --- | --- | --- |
| `toolbar/*`（左 / 右） | 行内横排 | 控件高取 sm(24) / md(28) 档，图标 12–16；单个贡献**不换行、不超一行**；放不下就用「图标 + `title` 提示」，别塞长文本 |
| `panelhead/status` | 行内 | 只放状态（文字 / 小徽标），不放大按钮；宽度尽量 < 160px |
| `panelhead/actions` | 行内 | 方形图标按钮取 **sm(24)** 档，建议最多 2–3 个 |
| `statusbar/canvas` | 行内 | 纯文本 / 小徽标，高度跟随状态栏（不要撑高） |
| `gutter/note` | 纵向 | 图标 12–16，不要撑宽 gutter |
| `titlebar/right` | 行内 | 图标按钮，贴合标题栏高度 |
| `settings/*` | 区块 | 纵向排列、**占满容器宽度**；区块自带标题与下间距（16 / 24）；不要自带外边距撑破设置页 |
| `contextmenu/*` | 菜单项 | 只提供 `{ label, onClick }`，由宿主菜单渲染，不要自绘 |

## 5. 状态与无障碍

- **状态不得只靠颜色**：色 + 图标或文字，至少两重编码。
- 键盘可达的元素用 `var(--focus-ring)` 作焦点样式。
- 正文对比度 ≥ 4.5:1，次要文本 ≥ 3:1。
- **纯图标按钮必须有可访问名**：用 `aria-label`（不要只靠 `title`——它不是可访问名，
  且读屏与视觉提示的表现不稳定）。带文字的按钮另可用 `title` 作补充说明。
- **加载指示**用内联的旋转环，不要引入自己的 spinner 图标；环宽取边长的 1/8 左右（不低于 1.5px）。
  默认环色：轨道 `var(--border)`、旋转顶弧 `var(--accent)`。**在强调色按钮内两者会同色而「看着不转」**，
  此时顶弧换成 `var(--accent-fg)`，轨道换成 `--accent` 与透明色的混（`color-mix`，见 §2）。