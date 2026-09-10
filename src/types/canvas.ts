/**
 * `.atlx` 文件 schema 类型（磁盘格式，atelyx-canvas/v1）。
 *
 * 与运行时 `types/node.ts` 分开：磁盘用扁平 x/y（不绑 React Flow），
 * 运行时↔磁盘的转换在 services/vault 层做。
 * 所有 node/edge/message 有稳定 id，为未来协作增量合并预留。
 */
import type { Message } from "./message";
import type { GlobalProvider, ReasoningEffort } from "./provider";
import type { LinkMode, SearchResultData } from "./node";
import type { CANVAS_SCHEMA } from "@/constants/canvas";

/** `.atlx` 文件根结构。 */
export interface CanvasFile {
  schema: typeof CANVAS_SCHEMA;
  id: string;
  title: string;
  nodes: CanvasFileNode[];
  edges: CanvasFileEdge[];
  createdAt: number;
  updatedAt: number;
}

/** 磁盘节点：扁平 x/y，data 按类型存不同结构。 */
export interface CanvasFileNode {
  id: string;
  type: "conversation" | "text" | "media" | "search" | "group" | "link" | "table";
  x: number;
  y: number;
  width?: number;
  /** 用户 NodeResizeControl 调整后的高度（缺省 = 内容自适应） */
  height?: number;
  data:
    | ConversationFileData
    | TextFileData
    | MediaFileData
    | SearchResultData
    | GroupFileData
    | LinkFileData
    | TableFileData;
}

/** 对话节点：messages 嵌在此处，随画布增量补丁（仅变化节点）落盘。 */
export interface ConversationFileData {
  providerId: string;
  model: string;
  /** 引用的 Agent 配置 id（仓库级 `.atelyx/agents.json`；缺省（未设置）= 按预置「对话」Agent 处理，旧文件无此字段兼容读取）。 */
  agentId?: string;
  /** LLM 自动生成的话题标题（首轮对话完成后命名；旧文件无此字段兼容读取）。 */
  title?: string;
  /** 节点级推理等级（缺省 = 不指定/跟随默认；旧文件无此字段兼容读取）。 */
  reasoningEffort?: ReasoningEffort;
  messages: Message[];
}

/** 文本节点：只存路径引用，正文在 笔记/*.md（可跨画布共享，删画布不删文件）。 */
export interface TextFileData {
  title: string;
  /** 相对仓库根的路径，如 `笔记/提示词-abc.md` */
  file: string;
}

/** 媒体节点：原文件在 附件/，此处存路径引用 + 元数据。 */
export interface MediaFileData {
  /** 相对仓库根的路径，如 `附件/image-xxx.png` */
  file: string;
  mime: string;
  kind: "image" | "file";
  /** 文件名（画布显示用） */
  name?: string;
  /** 二进制类解析失败时标注，仅作画布参考、不注入模型 */
  parseFailed?: boolean;
  /** 文本类文件解析出的内容（@ 引用/连边时注入用） */
  body?: string;
  /** 按图片真实比例计算的展示宽度（px），用户 resize 后此字段不再生效 */
  displayWidth?: number;
  /** 用户是否手动 resize 过此节点 */
  userResized?: boolean;
}

/** 分组节点：画布背景矩形容器（色板 1-6 只读展示，无改色 UI）。 */
export interface GroupFileData {
  /** 分组标题（双击 inline 编辑） */
  label: string;
  /** 色板下标（"1"-"6"，缺省 = 默认中性色） */
  color?: string;
}

/** 链接节点：URL 卡片，点击外部浏览器打开。 */
export interface LinkFileData {
  url: string;
}

/** 表格节点：引用 `.atb` 表格文件（快照不落盘，内容在独立文件，可跨画布共享）。 */
export interface TableFileData {
  title: string;
  /** 相对仓库根的 .atb 路径，如 `项目A/分镜.atb` */
  file: string;
}

/** 关联边（directed: false）的箭头模式：无向 / 单向 / 双向。缺省 = 无向（LinkMode 定义见 node.ts）。 */
export interface CanvasFileEdge {
  id: string;
  source: string;
  target: string;
  sourceHandle?: string;
  targetHandle?: string;
  /** false = 关联边（无消费语义）；缺省 true = 数据流边 */
  directed?: boolean;
  /** 关联边的箭头模式（仅 directed: false 生效；缺省 = 无向） */
  linkMode?: LinkMode;
  createdAt: number;
}

/**
 * 增量保存补丁（patch_canvas_vault，自动保存主路径）：只含变化/新增/删除的实体。
 * 前端按「上次保存快照引用 diff」计算（store 不可变更新，未变实体引用相同）；
 * Rust 按稳定 id 合并到磁盘全量文件——为未来协作按 id 增量合并铺路。
 */
export interface CanvasPatch {
  /** 画布 id（防串文件守卫）。 */
  id: string;
  /** 标题变化时更新（title 变更 = 同目录改文件名，写盘后返回新相对路径）。 */
  title?: string;
  upsertNodes: CanvasFileNode[];
  removedNodeIds: string[];
  upsertEdges: CanvasFileEdge[];
  removedEdgeIds: string[];
}

/** 画布列表行（递归扫描全仓库 .atlx 得到，不含拓扑）。 */
export interface CanvasFileRow {
  id: string;
  title: string;
  /** 相对仓库根的 .atlx 路径（画布任意文件夹存放，打开/保存按路径） */
  file: string;
  updatedAt: number;
}

/** `create_canvas_vault` 返回值：id（运行时身份）+ file（磁盘定位）。 */
export interface CanvasCreateResult {
  id: string;
  file: string;
}

/** `delete_folder` 返回值：空目录直接删；非空且未带 force 时 needsConfirm 供前端弹窗后重试。 */
export interface DeleteFolderResult {
  deleted: boolean;
  needsConfirm: boolean;
  /** 目录内条目数（递归计数，含子目录与隐藏文件）。 */
  itemCount: number;
}

/** 仓库文件树节点（`list_vault_tree`，文件面板全仓库树）。 */
export interface FileTreeNode {
  /** 文件名 / 文件夹名 */
  name: string;
  /** 相对仓库根路径（目录不带尾部分隔符；空串 = 仓库根自身） */
  path: string;
  isDir: boolean;
  /** mtime unix 秒 */
  updatedAt: number;
  children: FileTreeNode[];
}

/** 文件面板排序方式（树节点含 mtime，故只提供文件名/编辑时间两类）。 */
export type FileExplorerSortKey = "name-asc" | "name-desc" | "mtime-desc" | "mtime-asc";

/** 仓库级配置（.atelyx/config.json，不含 API key）。
 * 主题/强调色/字号/字体/自动恢复开关为应用级（global.json，见 GlobalConfig）。 */
export interface VaultConfig {
  /** 仓库级默认模型（从仓库内已配置供应商的全部模型中选择；缺省 = 未指定，跟随默认的对话请求报错提示）。 */
  model?: string;
  /** 仓库级默认模型所属供应商（与 model 配对固定供应商；旧配置缺省 = 按 model 名反查首个命中，重选后落盘）。 */
  modelProviderId?: string;
  /** 文件面板排序方式（缺省 = 前端默认 mtime-desc）。 */
  fileExplorerSort?: FileExplorerSortKey;
  /** 文件面板排除的文件夹名（任何层级同名文件夹不显示/不监听；设置页逗号分隔输入转数组）。 */
  excludeFolders?: string[];
  /** 附件导入默认文件夹（相对仓库根，可含子路径如 `assets/img`；缺省/空 = 仓库根目录）。 */
  attachmentFolder?: string;
  /** 仓库级 AI 供应商列表（磁盘格式默认无 key；key 走 keychain 条目 `provider-<vaultId>-<id>`，开启 syncKeys 后随仓库落盘）。 */
  providers?: GlobalProvider[];
  /** 仓库级搜索源配置（Tavily key 默认走 keychain 条目 `provider-<vaultId>-search-tavily`；开启 syncKeys 后随仓库落盘）。 */
  search?: GlobalSearchConfig;
  /** API key 是否随仓库保存（多设备同步）：开启后 provider/Tavily key 明文写入本文件，随仓库同步；
   * 缺省 false = key 仅存本机 keychain（按仓库隔离）。开启有泄露风险（仓库被公开/云盘共享）。 */
  syncKeys?: boolean;
  /** 宽松换行（仓库级，缺省 true）。 */
  softLineBreak?: boolean;
  /** 话题自动命名开关（缺省 = false 不启用）。 */
  autoNamingEnabled?: boolean;
  /** 话题自动命名模型（缺省 = 跟随默认模型；指定后命名用该模型，如 `{ providerId, model }`——话题命名一般用小模型）。 */
  autoNamingModel?: { providerId: string; model: string };
  /** 仓库稳定 ID（首次 open_vault 生成、之后固定；keychain 条目按它隔离，写盘必须保留）。 */
  vaultId?: string;
}

/** open_vault 返回的仓库信息。 */
export interface VaultInfo {
  /** 仓库根绝对路径 */
  root: string;
  /** 仓库名（文件夹名） */
  name: string;
  /** 仓库稳定 ID（`.atelyx/config.json` 的 vaultId，首次打开生成、之后固定；仓库归属识别用）。 */
  id: string;
}

/** 最近打开的仓库（存全局 global.json，启动页展示）。 */
export interface RecentVault {
  /** 仓库根绝对路径 */
  root: string;
  /** 仓库名（文件夹名） */
  name: string;
  /** 最近打开时间（unix 秒） */
  lastOpenedAt: number;
}

/** 反链行：引用方笔记的相对仓库根路径 + 标题（scan_wiki_backlinks 返回）。 */
export interface BacklinkRow {
  file: string;
  title: string;
}

/** 重建内部链接的结果统计（rebuild_internal_links 返回）。 */
export interface RebuildLinksResult {
  /** 扫描的 .md 文件数 */
  scanned: number;
  /** 实际写回修改的文件数 */
  modified: number;
  /** 改写的链接处数 */
  links: number;
}

export type SearchProvider = "tavily" | "searxng";

/** 仓库级搜索源配置（.atelyx/config.json 的 VaultConfig.search）。
 * Tavily key 默认走 keychain 条目 `provider-<vaultId>-search-tavily`，不落文件；
 * 仅 syncKeys 开启时随仓库落盘 `tavilyApiKey`（多设备同步）。 */
export interface GlobalSearchConfig {
  /** 搜索源：Tavily API / SearXNG 自建实例。 */
  provider: SearchProvider;
  /** SearXNG 实例 URL（tavily 模式忽略）。 */
  searxngUrl: string;
  /** Tavily API key（仅 syncKeys 开启时随仓库落盘/读取；关闭时剥离不写）。 */
  tavilyApiKey?: string;
}

/**
 * 全局配置（app_data_dir/global.json）——**应用级配置**：最近仓库列表 + 自动更新开关 +
 * 界面外观（主题/强调色/字号/字体）+ 自动恢复上次打开文件。
 * AI 供应商 / 搜索源已仓库化（`.atelyx/config.json` 的 `VaultConfig.providers/search`）；
 * 应用级 UI 使用状态（布局/上次打开文件/展开）走 `app_data_dir/ui-state.json`；
 * 不含 API key（key 仅存 keychain，条目按仓库隔离，见安全红线）。
 */
export interface GlobalConfig {
  /** 最近打开的仓库列表（按最近打开倒序，前端维护顺序） */
  recentVaults: RecentVault[];
  /** 自动检查更新（应用级）：开启后每次启动应用静默检查新版本并自动安装。缺省 = false（关闭）。 */
  autoUpdate?: boolean;
  /** 激活的主题插件 id（global.json）。缺省 = 默认主题插件；
   * 主题条目/设置项见 utils/pluginTheme 与 types/plugin.ts。 */
  theme?: string;
  /** 各主题插件的设置项值字典（键 = 插件 id；预置键 colorMode/accentColor + 插件自定义键）。
   * 缺省 = 默认主题插件 { colorMode: "system" }。 */
  themeSettings?: Record<string, Record<string, unknown>>;
  /** 应用级界面基础字号（px，覆盖 :root font-size；缺省 = 18）。 */
  fontSize?: number;
  /** 应用级界面字体（CSS font-family 值；缺省 = system-ui 默认）。 */
  fontFamily?: string;
  /** 进入仓库时自动恢复上次打开的文件（画布/笔记/表格）。缺省 = true（开启）。 */
  autoRestoreFiles?: boolean;
  /** 进入仓库时自动切到「主页」布局。缺省 = false（保持恢复上次界面）。 */
  defaultHomeLayout?: boolean;
  /** 协作中转（collab-relay）开关。缺省 = false（关闭）。 */
  collabEnabled?: boolean;
  /** 协作中转地址（如 ws://192.168.1.10:17701/ws）。 */
  collabRelayUrl?: string;
  /** 协作显示昵称（空 = 设备名兜底）。 */
  collabNickname?: string;
  /** 协作身份色（hex；空 = 随机分配）。 */
  collabColor?: string;
}

// ===== 外部白板格式（.canvas JSON，只读查看/转换为画布用）=====
// 与 .atlx 不同：file/text/group/link 四类节点 + 无向边（fromNode→toNode + 边锚点 side）。
// 本应用不写该格式（只读 + 转换），映射规则见 utils/whiteboard.ts。

/** `.canvas` 文件根结构（文件路径为相对仓库根，如 `项目A/白板.canvas`）。 */
export interface WhiteboardFile {
  nodes: WhiteboardNode[];
  edges: WhiteboardEdge[];
  /** 视口（x/y/zoom，本应用不读写） */
  view?: { x: number; y: number; zoom: number };
  /** 画布背景色板（本应用不读写） */
  canvas?: string;
}

/** `.canvas` 节点：`file`（.md/附件引用）、`text`（文本）、`group`（分组）、`link`（URL）。 */
export interface WhiteboardNode {
  id: string;
  type: "file" | "text" | "group" | "link";
  /** file 节点：相对仓库根路径；link 节点：URL */
  file?: string;
  text?: string;
  label?: string;
  url?: string;
  x: number;
  y: number;
  width?: number;
  height?: number;
  /** 色板下标（"1"-"6"，本应用仅 group 展示使用） */
  color?: string;
}

/** `.canvas` 边：无向，`fromSide`/`toSide` = 锚点方位（top/right/bottom/left）。 */
export interface WhiteboardEdge {
  id: string;
  fromNode: string;
  fromSide?: string;
  toNode: string;
  toSide?: string;
  label?: string;
  color?: string;
}
