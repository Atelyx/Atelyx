/**
 * AI 工具（Agent 模式）UI 元数据 + 默认勾选 + read_file 分页执行常量。
 *
 * 这里放**组件可见**的展示元数据（id/label/依赖项）与 read_file 分页默认行数（工具/服务共用）；
 * 工具的可执行定义（schema/参数校验/摘要/执行/回填）在 `services/ai/tools/*`。两者以 `id`
 * （= 工具名）为联结键。Agent 配置里勾选的工具以 id 列表存 `.atelyx/agents.json`。
 *
 * 工具为基础文件/网络能力（对仓库内任意文本文件与网页生效），命名通用规范。
 */
/** 工具分类键（Agent 设置页折叠分组；顺序见 AGENT_TOOL_CATEGORIES）。 */
export type AgentToolCategory = "web" | "file" | "task" | "plugin";

export interface AgentToolMeta {
  id: string;
  label: string;
  /** 分类（设置页折叠分组依据）。 */
  category: AgentToolCategory;
  /** 依赖搜索源配置（未配置时自动剔除并提示）。 */
  needsSearch?: boolean;
  /** 只读工具：默认开启、可取消勾选（取消后模型不再拥有该工具）。 */
  readOnly?: boolean;
}

/** read_file 单次默认/最大返回行数（分页读取，模型可传 offset 继续读大文件）。
 * 单行字符/单次字节预算上限由 Rust 侧 `read_vault_file_window` 强制（见 commands/vault.rs），
 * 前端无需重复定义。 */
export const READ_WINDOW_DEFAULT_LINES = 2000;

/** glob 单次内联返回路径上限 / grep 单次内联返回匹配上限 / grep 单行预览字节上限 / list_dir 单层条目上限。
 * 均由 Rust 侧强制（filesearch.rs 的 glob_vault/grep_vault、commands/vault.rs 的 list_vault_dir），
 * 此处仅用于工具描述文案，口径与 READ_WINDOW_DEFAULT_LINES 一致。 */
export const GLOB_MAX_RESULTS = 100;
export const GREP_MAX_MATCHES = 250;
export const GREP_MAX_LINE_BYTES = 2000;
export const LIST_DIR_MAX_ENTRIES = 200;

/** 单轮内并行安全的工具调用最大在飞数（有界滚动池；1 = 全串行）。 */
export const MAX_PARALLEL_TOOL_CALLS = 10;

/** 折叠历史工具结果中段时保留的头部字符数（提供开头线索：路径、起始行）。 */
export const TOOL_RESULT_PRUNE_HEAD_CHARS = 4096;

/** 折叠历史工具结果中段时保留的尾部字符数（提供结尾线索：页脚、结论）。 */
export const TOOL_RESULT_PRUNE_TAIL_CHARS = 1024;

/** 历史工具结果超过此字符数才折叠中段（随请求重发时用）。
 * 不变式：头 + 尾 ≤ 阈值，否则折叠后不会变小。 */
export const TOOL_RESULT_PRUNE_THRESHOLD_CHARS = 8192;

/** 替换被折叠中段的固定标记（模型据此知道内容不完整，可按需重新读取）。 */
export const TOOL_RESULT_PRUNE_MARKER = "\n\n[... tool result middle pruned ...]\n\n";

/** 相对路径是否含隐藏段：任一段以 `.` 开头（如 `.atelyx/x`、`a/.git/y`、根级 `.gitignore`）。
 * AI 工具完全屏蔽前面带 `.` 的目录/文件，此判定供各工具 validate 兜底（Rust 发现层另有过滤）。
 * 排除 `..`（父目录段）——它由路径穿越校验（safe_join「含越界段」）拒绝，报错语义更准确。 */
export function hasHiddenSegment(path: string): boolean {
  return path
    .split(/[\\/]+/)
    .some((seg) => seg.startsWith(".") && seg.length > 1 && seg !== "..");
}

/** 隐藏段被拒时的统一报错文案（各文件工具 validate 共用）。 */
export const HIDDEN_PATH_ERROR = "路径位于隐藏目录/文件（. 开头段），AI 工具不可访问";

/** web_fetch 结果标题预览长度上限（气泡摘要/空正文提示里截断标题用）。 */
export const WEB_FETCH_TITLE_PREVIEW = 80;

/** UI 展示与默认勾选的工具名单（执行层同名集合见 services/ai/tools）。
 * 分类 = 设置页折叠分组；只读工具默认开启、可取消，取消后从模型名册移除。 */
export const AGENT_TOOLS_META: AgentToolMeta[] = [
  { id: "web_search", label: "联网搜索", category: "web", needsSearch: true },
  { id: "web_fetch", label: "抓取网页", category: "web" },
  { id: "read_file", label: "读取文件", category: "file", readOnly: true },
  { id: "glob", label: "查找文件", category: "file", readOnly: true },
  { id: "grep", label: "搜索内容", category: "file", readOnly: true },
  { id: "list_dir", label: "列出目录", category: "file", readOnly: true },
  { id: "read_history", label: "读取历史", category: "file", readOnly: true },
  { id: "edit_file", label: "编辑文件", category: "file" },
  { id: "append_file", label: "追加内容", category: "file" },
  { id: "write_file", label: "写入文件", category: "file" },
  { id: "rename_file", label: "重命名文件", category: "file" },
  { id: "move_file", label: "移动文件", category: "file" },
  { id: "delete_file", label: "删除文件", category: "file" },
  { id: "delete_dir", label: "删除目录", category: "file" },
  { id: "todo_write", label: "任务清单", category: "task" },
];

/** 工具分类展示顺序（Agent 设置页折叠分组标题）。 */
export const AGENT_TOOL_CATEGORIES: { key: AgentToolCategory; label: string }[] = [
  { key: "web", label: "联网" },
  { key: "file", label: "文件" },
  { key: "task", label: "任务清单" },
  { key: "plugin", label: "插件" },
];

/** 只读工具 id 集合：预置「对话」Agent 默认补全。 */
export const READONLY_TOOL_IDS = AGENT_TOOLS_META.filter((t) => t.readOnly).map((t) => t.id);

/** Agent 默认启用的工具 id（全部工具，新建 Agent 与预置「Agent」默认全开）。 */
export const DEFAULT_AGENT_TOOLS = AGENT_TOOLS_META.map((t) => t.id);

/**
 * 系统提示词引导（工具含 read_file 时追加，随每条请求进 system 消息）：
 * @引用 的笔记只带文件路径，模型需用 read_file 按路径读取正文，而不是猜测内容；
 * 目录引用（/ 结尾）先 list_dir 列内容再按需读取。已读取过的文件内容在对话历史中，
 * 明示「无需重读」以免模型重复读取同一文件。
 */
export const FILE_REFERENCE_PROMPT =
  "以 @ 前缀引用的文件是用户明确指定的笔记，其相对仓库根路径列在消息开头「引用文件」列表中。需要其内容时用 read_file 工具读取；在读取之前不要声称已查看过该文件。以 / 结尾的引用是目录：先用 list_dir 工具列出其中的内容（子目录会给出子项数），再按需 read_file；glob 用于按模式检索文件。以 . 开头的目录/文件（如 .git、.atelyx）不可被 AI 工具访问。已读取过的文件内容在对话历史中，无需重复读取；仅在需要确认内容是否变更时重读。";
