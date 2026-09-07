/**
 * 插件平台契约：清单 / 市场索引 / 能力 / 皮肤 / 运行状态。
 *
 * 这是分布式插件（任何来源、任何作者）与 App 之间的唯一数据契约。契约带格式版本号：
 * 未知的字段、类型、能力名一律跳过而不报错，保证「更新的插件、更老的 App」也能安全共处；
 * 反向（更老的插件、更新的 App）由插件自身的宿主兼容范围字段约束。
 */
import type { TableField, TableRow } from "./table";

/** 清单格式版本：升级清单结构时递增；App 拒绝 schemaVersion 大于当前值的清单。 */
export const PLUGIN_SCHEMA_VERSION = 1;

/** 插件可声明的全部能力/命令名（清单 uses 字段取值 + 运行时门槛共用，单一数据源）。 */
export const PLUGIN_CAPABILITIES = [
  // 敏感能力：未声明即运行时拒绝（声明了就能用，不额外弹窗）。
  "keychain:read", // 读 API key
  "shell", // 执行外部进程
  "vault:delete", // 删除仓库文件
  // 常规能力。
  "vault:read", // 读仓库文件
  "vault:write", // 写仓库文件
  "vault:rename", // 重命名/移动仓库文件
  "ai:chat", // 发起 AI 对话
  "ai:tool", // 注册 AI 工具
  "search:web", // 联网搜索
  "web:fetch", // 抓取网页
  "clipboard", // 剪贴板读写
  "window:manage", // 窗口控制（新建/撕裂）
  "settings:read", // 读配置
  "settings:write", // 写配置
  "state:persist", // 自持数据落盘
  "events:subscribe", // 订阅应用事件
  "table:read", // 读当前表格数据（行/字段/图片/选中行）
] as const satisfies readonly string[];

/** 插件可声明的能力/命令全集。 */
export type PluginCapability = (typeof PLUGIN_CAPABILITIES)[number];

/** 敏感能力名单（未声明即运行时拒绝的子集）。 */
export const SENSITIVE_PLUGIN_CAPABILITIES = [
  "keychain:read",
  "shell",
  "vault:delete",
] as const satisfies readonly PluginCapability[];

/** 敏感能力类型。 */
export type SensitivePluginCapability = (typeof SENSITIVE_PLUGIN_CAPABILITIES)[number];

/**
 * 插件展示分类：type 只做市场展示/过滤，实际能力在运行时经桥注册（一个插件可属多类）。
 * 新增分类不破坏旧 App：旧 App 遇到未知 type 会在市场/安装时安全跳过。
 */
export type PluginType =
  | "tool" // AI 工具/命令（模型可调用）
  | "setting" // 设置页条目
  | "panel" // 工作区内面板视图
  | "app" // 应用级页面/模式（主页接管、全页）
  | "node" // 画布节点
  | "theme" // UI 皮肤（CSS 变量覆盖）
  | "command" // 全局动作/菜单/快捷键
  | "background" // 后台常驻服务（无界面）
  | "tableview"; // 表格编辑器内的多维表格视图（registerTableView）

/** 安装作用域：app=个人工具（本机，默认）；vault=随仓库共享。 */
export type PluginScope = "app" | "vault";

/** 插件安装来源类型（管理 UI 徽标/更新可用性依据）。 */
export type PluginSourceKind = "market" | "git" | "local";

/** 声明式皮肤：覆盖 CSS 变量（无需运行时代码；键可带或省略 `--` 前缀，应用时统一补前缀）。 */
export interface PluginTheme {
  /** 主题变量覆盖（如 { "--accent": "#7c3aed" }）。 */
  variables: Record<string, string>;
  /** 暗色模式下的额外覆盖（可选，浅色覆盖之上叠加）。 */
  dark?: Record<string, string>;
}

/** 插件清单（插件根目录的 atelyx.json）。 */
export interface PluginManifest {
  /** 清单格式版本（= PLUGIN_SCHEMA_VERSION）。 */
  schemaVersion: number;
  /** 反向域名式稳定标识，发布后不可变。 */
  id: string;
  /** 显示名。 */
  name: string;
  /** 语义化版本（x.y.z）。 */
  version: string;
  /** 主分类（市场展示/过滤）。 */
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
  /** 声明的能力/命令使用清单：市场展示 + 敏感能力门槛依据。 */
  uses?: PluginCapability[];
  /** 权限说明：能力名 → 一句理由（安装/详情展示）。 */
  permissions?: Record<string, string>;
  /** 声明式皮肤（type 为 theme 时通常携带；应用层按启用顺序叠加）。 */
  theme?: PluginTheme;
  /** 入口 JS（相对插件根目录；worker 平面：tool/background/command 逻辑；纯 theme 插件可省略）。 */
  main?: string;
  /** 主线程 UI 入口（相对插件根目录；可选：UI 类插件在此声明，与 main 并存时双平面加载）。 */
  mainUi?: string;
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
  /** 命中封禁名单时的原因。 */
  blockedReason?: string;
}

/** 市场索引（index.json）。 */
export interface PluginIndex {
  /** 生成时间（ISO）。 */
  generatedAt: string;
  /** 索引格式版本。 */
  version: string;
  items: PluginIndexEntry[];
}

/** 已装插件的运行阶段。 */
export type PluginFiberPhase = "pending" | "loading" | "active" | "failed";

/** 已装插件运行记录（pluginStore 用）。 */
export interface InstalledPlugin {
  id: string;
  manifest: PluginManifest;
  /** 归一化作用域（缺省 app）。 */
  scope: PluginScope;
  /** 安装目录（Rust 返回的绝对路径；本地来源为链接路径）。 */
  installDir: string;
  /** 安装来源类型（市场 / Git / 本地目录）。 */
  sourceKind: PluginSourceKind;
  enabled: boolean;
  phase: PluginFiberPhase;
  /** 桥实际调用过的能力（内存审计，上限截断）。 */
  usedCapabilities: string[];
  /** 加载失败原因。 */
  error?: string;
  /** 命中市场封禁名单的原因（已装插件被下架标记；管理 UI 据此禁启提示）。 */
  blocked?: string;
}

/**
 * 插件表格数据快照（主线程 facade `subscribeTableData` 推送；结构即契约）。
 * 主线程同域直传 store 的不可变数组引用（选中/状态变化不重建 rows/fields，插件可据此 memo 隔离），
 * worker 平面若复用本契约须自行序列化。
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
