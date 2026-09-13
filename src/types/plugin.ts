/**
 * 插件平台契约：插件包 / 市场索引 / 能力 / 皮肤 / 运行状态。
 *
 * 这是分布式插件（任何来源、任何作者）与 App 之间的唯一数据契约。插件包 = 插件根目录的
 * `package.json`（npm 标准字段 + 嵌套 `atelyx` 块）+ 单入口（默认导出 `apply(ctx)`）。
 * 未知的字段、类型、附加分类一律跳过而不报错（主分类错误则拒绝，见 utils/pluginManifest）。
 */
import type { TableField, TableRow } from "./table";

/** 插件展示分类：type 只做市场展示/过滤，实际能力在运行时经 apply 注册（一个插件可属多类）。
 *  新增分类不破坏旧 App：旧 App 遇到未知 type 会在市场/安装时安全跳过。 */
export type PluginType =
  | "tool" // AI 工具/命令（模型可调用）
  | "setting" // 设置页条目
  | "panel" // 工作区内面板视图
  | "app" // 应用级页面/模式（主页接管、全页）
  | "node" // 画布节点
  | "theme" // UI 皮肤（CSS 变量覆盖）
  | "command" // 全局动作/菜单/快捷键
  | "background" // 后台常驻服务（无界面）
  | "tableview"; // 表格编辑器内的多维表格视图

/** 安装作用域：app=个人工具（本机，默认）；vault=随仓库共享。 */
export type PluginScope = "app" | "vault";

/** 插件来源（中性信息：展示徽标 + 更新渠道分派；不构成类别，不参与装配/权限判定）。 */
export type PluginSourceKind = "market" | "git" | "local" | "builtin";

/**
 * 声明式主题条目：基础配色方案 + 语义变量覆盖（无需运行时代码；键可带或省略 `--` 前缀，
 * 应用时统一补前缀）。主题只覆盖想改的变量子集，未覆盖的落回基础方案（colorScheme 决定的
 * 浅/深基础条目），保证对比度与完整性兜底。
 */
export interface ThemeDefinition {
  /** 主题条目 id（插件内唯一；与基础条目 `light`/`dark` 重名会被丢弃，插件其余条目仍生效）。 */
  id: string;
  /** 显示名（主题选择列表展示）。 */
  name: string;
  /** 基础配色方案：决定 `.dark` class / color-scheme / 原生控件配色。 */
  colorScheme: "light" | "dark";
  /** 语义变量覆盖（如 { "--bg-primary": "#f0f0f0" }）。 */
  variables: Record<string, string>;
}

/**
 * 主题插件设置项声明：预置设置项类型（由内核实现并应用，无需插件代码）。
 * 自定义设置项经主线程 UI 平面 registerThemeSetting 运行时注册。
 */
export interface PluginThemeOptions {
  /** 使用内核预置「强调色」设置项（值自动应用到 --accent 系列；存 themeSettings[插件id].accentColor）。 */
  accent?: boolean;
}

/**
 * 插件包原始清单（插件根目录 `package.json` 的原始形状）：`name` = 插件 id（反向域名）、
 * `version`、`main` + 嵌套 `atelyx` 块（显示名/类型/作用域/披露/主题声明等）。
 * 跨 Rust 边界的形态（列表返回行与默认组合播种都用它）；行对象经 `utils/pluginManifest`
 * 归一化为 `PluginManifest` 后供前端消费。
 */
export type PluginPackageJson = Record<string, unknown>;

/**
 * 插件包清单（插件根目录的 package.json 归一化；原始输入为 npm 标准字段 + `atelyx` 块，
 * 归一化后展平为本类型，见 utils/pluginManifest）。id = package.json 的 name（反向域名）。
 */
export interface PluginManifest {
  /** 反向域名式稳定标识（= package.json 的 name），发布后不可变。 */
  id: string;
  /** 显示名（package.json atelyx.name，缺省 = id）。 */
  name: string;
  /** 语义化版本（package.json version）。 */
  version: string;
  /** 主分类（市场展示/过滤；package.json atelyx.type）。 */
  type: PluginType;
  /** 全部分类（含主分类，去重；缺省 = [type]）。 */
  types?: PluginType[];
  /** 安装作用域，缺省 app。 */
  scope?: PluginScope;
  /** 兼容的宿主版本下限（缺省不限制）。 */
  atelyxVersionMin?: string;
  /** 兼容的宿主版本上限（不含，缺省不限制）。 */
  atelyxVersionMax?: string;
  /** 目标平台（如 windows-x64 / linux-x64），缺省全平台。 */
  platforms?: string[];
  /** 入口（相对插件根目录；.js/.ts/.tsx；纯 theme 插件可省略）。 */
  main?: string;
  /** 披露：将访问的 Atelyx 服务名（管理页「声明 vs 实际」审计对照的声明侧；无运行时门槛）。 */
  declares?: string[];
  /** 权限说明：服务名 → 一句理由（安装/详情展示）。 */
  permissions?: Record<string, string>;
  /** 声明式主题条目（type 含 theme 时通常携带；必须 ≥1；id 插件内唯一）。 */
  themes?: ThemeDefinition[];
  /** 主题设置项声明（预置类型：accent = 内核实现的强调色设置项）。 */
  themeOptions?: PluginThemeOptions;
  /** 插件契约版本（宿主 App 版本解耦；不兼容时加载时响亮拒绝）。 */
  hostApiVersion?: number;
  /** 一句简介。 */
  tagline?: string;
  /** 详细描述（markdown）。 */
  description?: string;
  /** 作者。 */
  author?: string;
  /** SPDX 许可（如 "MIT"）。 */
  license?: string;
  /** 分类标签。 */
  tags?: string[];
}

/** 市场徽标：official=官方（按账号自动判定）；endorsed=精选（人工授予）。 */
export type PluginBadge = "official" | "endorsed";

/** 市场索引条目：发现元数据 + 来源定位（安装/更新取源码：git clone，无 git 回退 GitHub 源码包）。 */
export interface PluginIndexEntry {
  id: string;
  name: string;
  tagline?: string;
  description?: string;
  /** owner/repo，下载与更新定位。 */
  repo: string;
  defaultBranch: string;
  stars: number;
  updatedAt: string;
  topics: string[];
  /** 主分类（从清单解析，缺省未知）。 */
  type?: PluginType;
  badge?: PluginBadge;
}

/** 市场索引（index.json）。 */
export interface PluginIndex {
  /** 生成时间（ISO）。 */
  generatedAt: string;
  /** 索引格式版本。 */
  version: string;
  items: PluginIndexEntry[];
}

/** 插件行的运行阶段。 */
export type PluginFiberPhase = "pending" | "active" | "failed";

/** 挂载失败阶段（按执行顺序：清单 → 兼容 → 读取 → 转译 → 求值 → 激活）。 */
export type PluginMountPhase = "manifest" | "compat" | "read" | "transpile" | "eval" | "apply";

/** 分段失败诊断（宿主内部与插件管理页用；插件侧不可见）。 */
export interface PluginMountFailure {
  phase: PluginMountPhase;
  message: string;
  /** phase = apply 且因 inject 依赖未满足时的缺失服务清单。 */
  missing?: string[];
}

/** 已装插件运行记录（pluginStore 用）。 */
export interface InstalledPlugin {
  id: string;
  manifest: PluginManifest;
  /** 归一化作用域（缺省 app）。 */
  scope: PluginScope;
  /** 安装目录（Rust 返回的绝对路径；本地来源为链接路径；实现随应用编译的行无磁盘目录，为空串）。 */
  installDir: string;
  /** 来源（中性信息）。 */
  sourceKind: PluginSourceKind;
  enabled: boolean;
  phase: PluginFiberPhase;
  /** 加载失败诊断（分段 phase + 可读原因 + 可选缺失服务清单）。 */
  failure?: PluginMountFailure;
  /** 可回退到的上一版本；仅代码回退，插件 data 保持当前内容。 */
  previousVersion?: string;
}

/**
 * 插件表格数据快照（ctx.table.snapshot 返回；结构即契约）。
 * rows/fields 为 store 的不可变数组引用（选中/状态变化不重建 rows/fields，插件可据此 memo 隔离）。
 */
export interface PluginTableSnapshot {
  /** 当前打开的 .atb 相对仓库根路径（null = 未打开表格）。 */
  tableFile: string | null;
  fields: TableField[];
  rows: TableRow[];
  /** 选中行（表格视图/插件视图联动；null = 无选中）。 */
  selectedRowId: string | null;
  /** 协作远端选中行 → 用户色（cell/range/row 区域归约；column/all 不染；首个匹配 peer 优先）。 */
  peerColorByRowId: Record<string, string>;
}

/**
 * 插件画布节点（`canvas` 能力投影；与磁盘/协作格式同构，JSON 可序列化）。
 * type 为开放字符串——插件可注册自定义节点类型，不限于内置类型。
 */
export interface PluginCanvasNode {
  id: string;
  type: string;
  x: number;
  y: number;
  width?: number;
  height?: number;
  /** 各类型 data；conversation 节点含 messages（与磁盘格式一致，消息数组随快照内嵌）。 */
  data: Record<string, unknown>;
}

/** 插件画布边（与磁盘/协作格式同构；directed 缺省 true=数据流边，false=关联自由线）。 */
export interface PluginCanvasEdge {
  id: string;
  source: string;
  target: string;
  sourceHandle?: string;
  targetHandle?: string;
  directed?: boolean;
  linkMode?: string;
}

/** 插件画布快照（`canvas` 能力 snapshot() 返回；canvasFile=null = 未打开画布）。 */
export interface PluginCanvasSnapshot {
  canvasFile: string | null;
  canvasTitle: string;
  nodes: PluginCanvasNode[];
  edges: PluginCanvasEdge[];
  selectedNodeId: string | null;
}
