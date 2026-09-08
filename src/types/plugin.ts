/**
 * 插件平台契约：清单 / 市场索引 / 能力 / 皮肤 / 运行状态。
 *
 * 这是分布式插件（任何来源、任何作者）与 App 之间的唯一数据契约。契约带格式版本号：
 * 未知的字段、类型、附加分类一律跳过而不报错（主分类错误则拒绝，见 utils/pluginManifest）；
 * 反向（更老的插件、更新的 App）由插件自身的宿主兼容范围字段约束。
 */
import type { TableField, TableRow } from "./table";
import type { CanvasFileRow, FileTreeNode } from "./canvas";

/** 清单格式版本：升级清单结构时递增；App 拒绝 schemaVersion 大于当前值的清单。 */
export const PLUGIN_SCHEMA_VERSION = 2;

/** 插件逻辑运行平面（UI 平面永远在主线程跑 JS；此处仅逻辑平面语言）。 */
export type PluginRuntime = "js" | "ts" | "python";

/** 静态贡献声明（纯元数据：市场/管理 UI 展示与发现；运行时行为经桥注册，二者不强制一致）。 */
export interface PluginContributes {
  /** 静态命令声明（id/label 展示用）。 */
  commands?: Array<{ id: string; label: string }>;
  /** 静态面板声明（kind/label 展示用）。 */
  panels?: Array<{ kind: string; label: string }>;
  /** 静态设置项声明（key/label 展示用）。 */
  settings?: Array<{ key: string; label: string }>;
}

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
export type PluginSourceKind = "market" | "git" | "local" | "builtin";

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
  /** 逻辑运行平面语言（缺省 js；UI 平面永远在主线程跑 JS）。 */
  runtime?: PluginRuntime;
  /** 提供的能力命名空间（反向域名，必含点；其他插件/宿主可经 bridge.call 调用）。 */
  provides?: string[];
  /** 依赖的能力命名空间（宿主或他插件提供；懒解析——缺失在调用时报错，不阻塞激活）。 */
  requires?: string[];
  /** 披露：将调用的能力命名空间（宿主命名空间如 state/shell，或他插件反向域名；
   *  与 provides 同一词汇表，市场展示 + 管理页审计对照；无运行时门槛）。 */
  declares?: string[];
  /** 静态贡献声明（纯元数据：市场/管理 UI 展示与发现）。 */
  contributes?: PluginContributes;
  /** 权限说明：能力名 → 一句理由（安装/详情展示）。 */
  permissions?: Record<string, string>;
  /** 声明式皮肤（type 为 theme 时通常携带；应用层按启用顺序叠加）。 */
  theme?: PluginTheme;
  /** 入口（相对插件根目录；js/ts 为脚本，python 为子进程入口；纯 theme 插件可省略）。 */
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
  /** 安装目录（Rust 返回的绝对路径；本地来源为链接路径；内置插件无磁盘目录，为空串）。 */
  installDir: string;
  /** 安装来源类型（市场 / Git / 本地目录 / 内置）。 */
  sourceKind: PluginSourceKind;
  enabled: boolean;
  phase: PluginFiberPhase;
  /** 桥实际调用过的能力（内存审计，上限截断）。 */
  usedCapabilities: string[];
  /** 加载失败原因。 */
  error?: string;
}

/**
 * UI 平面插件经 facade 获得的仓库访问契约（文件树 + 打开入口）。
 * 经 `setPluginVaultAccess` provider 注入（见 services/plugins/ui.ts，pluginStore 接线），
 * 任何面板插件可用，与内置搜索面板同一输入面。
 */
export interface VaultAccess {
  /** 读取当前仓库文件树（调用时取当下快照）。 */
  listFiles(): Promise<FileTreeNode[]>;
  /** 打开画布（.atlx/.canvas 行，与文件面板同一入口）。 */
  openCanvasFile(row: CanvasFileRow): void;
  /** 打开笔记窗口。 */
  openNote(file: string, title: string): void;
  /** 打开表格窗口。 */
  openTable(file: string, title: string): void;
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
