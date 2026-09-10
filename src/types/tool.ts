/**
 * AI 工具契约。
 *
 * - `ToolSchema`：发给模型的工具名册描述（中性，适配器转供应商 tools 字段）。
 * - `ToolDefinition`：自包含工具模块的完整定义。schema/参数校验/摘要/执行/结果回填
 *   全部收敛在一个文件里，新增工具 = 一个 defineTool 模块 + 注册，不再改 switch。
 * - `ToolExecContext`：执行上下文，能力经 `capabilities` 注入（运行时由调用方注入），
 *   工具本身不直接 import store —— 保证可移植、可复用。
 */
import type { SearchResultData } from "./node";

/** 发给模型的工具名册描述（对应 OpenAI function 定义）。 */
export interface ToolSchema {
  name: string;
  description: string;
  /** JSON Schema（arguments）。 */
  parameters: Record<string, unknown>;
}

/** 一次工具执行结果（可视化 + 回填模型用）。 */
export interface ToolResult {
  ok: boolean;
  /** 气泡工具块展示的短摘要。 */
  summary: string;
  /** 要回填给模型的文本（缺省由 ToolDefinition.renderResult 决定）。 */
  content?: string;
  /** 供调用方 hooks 消费的产物数据（如画布建节点）。 */
  data?: unknown;
}

/** todo 清单条目（todo_write 工具的整单替换单元；status 枚举与 UI 一致）。 */
export interface TodoItem {
  content: string;
  status: "pending" | "in_progress" | "completed";
}

/** read_history 工具结果：未指定 version 时返回版本摘要列表（不带全文防撑爆上下文），
 * 指定 version 时返回该版全文快照（供模型 write_file 回写恢复）。 */
export interface AgentHistoryReadResult {
  ok: boolean;
  /** 气泡摘要（成功 = 版本数/目标版本；失败 = 具体原因）。 */
  summary: string;
  /** 版本摘要列表（作者名/行为/改动摘要/备注，不含快照正文）。 */
  versions?: Array<{
    seq: number;
    ts: number;
    authorName: string;
    action: string;
    summary?: string;
    note?: string;
  }>;
  /** 指定 version 时的全文快照。 */
  content?: string;
}

/** read_file 分页窗口的一行（number = 文件内 1-based 绝对行号）。 */
export interface ReadWindowLine {
  number: number;
  text: string;
}

/** read_file 分页窗口结果（对应 Rust `read_vault_file_window`，camelCase 序列化）。 */
export interface ReadWindowResult {
  lines: ReadWindowLine[];
  /** 文件精确总行数（页脚引导用，即便窗口未读全）。 */
  totalLines: number;
  /** 单次返回是否命中字节预算被截断（模型应继续分页/缩小窗口）。 */
  truncated: boolean;
}

/** glob 检索结果（对应 Rust `glob_vault`，camelCase 序列化）。 */
export interface GlobVaultResult {
  /** 搜索基准：path 参数原样（缺省 = 空串 = 仓库根）。 */
  root: string;
  /** 命中的文件路径（相对仓库根、`/` 分隔；按修改时间升序，最多内联上限条）。 */
  paths: string[];
  /** 全部命中数（可能大于 paths.length，超上限时用于提示收窄）。 */
  total: number;
  /** 是否因超上限被截断（total > paths.length）。 */
  capped: boolean;
}

/** grep 匹配行（对应 Rust `GrepMatchRow`，camelCase 序列化）。 */
export interface GrepMatchRow {
  /** 相对仓库根路径（`/` 分隔）。 */
  path: string;
  /** 文件内 1-based 行号。 */
  lineNumber: number;
  /** 行内容（超长已按字节截断并附后缀）。 */
  line: string;
}

/** grep 检索结果（对应 Rust `grep_vault`）。 */
export interface GrepVaultResult {
  /** 内联返回的匹配行（最多上限条，行号升序、按文件连续）。 */
  matches: GrepMatchRow[];
  /** 全部匹配数（可能大于 matches.length，超上限时用于提示收窄）。 */
  total: number;
  /** 是否因超上限被截断（total > matches.length）。 */
  capped: boolean;
}

/** list_dir 单层条目（对应 Rust `list_vault_dir`，camelCase 序列化）。 */
export interface ListDirEntry {
  name: string;
  /** "dir" | "file"。 */
  kind: "dir" | "file";
  /** 文件字节大小（仅文件有）。 */
  size?: number;
  /** 目录直接子项数（仅目录有）。 */
  children?: number;
}

/** list_dir 结果（entries/total/capped 词汇与 glob/grep 一致）。 */
export interface ListDirResult {
  /** 内联返回的条目（目录在前、按名称升序，最多上限条）。 */
  entries: ListDirEntry[];
  /** 目录真实条目总数（可能大于 entries.length，超上限时用于提示收窄）。 */
  total: number;
  /** 是否因超上限被截断（total > entries.length）。 */
  capped: boolean;
}

/** 工具执行所需的能力缝（由调用方注入；工具依赖此而非 store）。 */
export interface ToolCapabilities {
  /** 联网搜索（依赖搜索源配置）。 */
  search?: (query: string) => Promise<SearchResultData>;
  /** 分页读仓库内任意文本文件：返回带绝对行号的窗口（offset 1-based 默认 1；limit 默认见常量）。 */
  readFile?: (
    path: string,
    opts?: { offset?: number; limit?: number },
  ) => Promise<ReadWindowResult>;
  /** 写仓库内任意文本文件（指定相对路径，直接落盘）。 */
  writeFile?: (path: string, content: string) => Promise<{ ok: boolean; summary: string }>;
  /** 行级修改仓库内任意文本文件（oldText 唯一匹配）。 */
  editFile?: (
    path: string,
    edits: Array<{ oldText: string; newText: string }>,
  ) => Promise<{ ok: boolean; summary: string }>;
  /** 按 glob 模式枚举仓库内文件路径（只返回文件，相对仓库根；上限内联 + total）。 */
  glob?: (
    pattern: string,
    opts?: { path?: string },
  ) => Promise<GlobVaultResult>;
  /** 正则搜索仓库内文件内容，返回匹配行（含行号与路径；上限内联 + total）。 */
  grep?: (
    pattern: string,
    opts?: { path?: string; include?: string },
  ) => Promise<GrepVaultResult>;
  /** 单层列出目录条目（目录在前、含隐藏项；子目录带子项数）。dir 缺省 = 仓库根。 */
  listDir?: (dir?: string) => Promise<ListDirResult>;
  /**
   * 同目录重命名仓库内文件（.md/.atb/.atlx 标题随文件名同步）。newName 须为纯文件名；
   * 目标重名自动加序号，actualPath 恒为实际落盘路径。失败 ok=false 不抛断整轮。
   */
  renameFile?: (
    oldPath: string,
    newName: string,
  ) => Promise<{ ok: boolean; summary: string; actualPath: string }>;
  /**
   * 移动仓库内文件到目标文件夹（保持文件名）。targetDir 空 = 仓库根；
   * 目标重名自动加序号，actualPath 恒为实际落盘路径。失败 ok=false 不抛断整轮。
   */
  moveFile?: (
    oldPath: string,
    targetDir: string,
  ) => Promise<{ ok: boolean; summary: string; actualPath: string }>;
  /** 按路径删除仓库内单个文件（.atb 连带删除其私有附件目录）。失败 ok=false 不抛断整轮。 */
  deleteFile?: (path: string) => Promise<{ ok: boolean; summary: string }>;
  /** 删除仓库内目录（force=true 递归删除非空目录）；needsConfirm 供工具转「需显式确认」提示。 */
  deleteDir?: (
    dir: string,
    force?: boolean,
  ) => Promise<{ ok: boolean; summary: string; needsConfirm: boolean; itemCount: number }>;
  /** 读某仓库文件的历史（read_history 工具；内部直读 .atelyx/history/，隐藏屏蔽的刻意豁免）。 */
  readHistory?: (path: string, opts?: { version?: number }) => Promise<AgentHistoryReadResult>;
  /** 追加内容到已存在的仓库文本文件（append_file 工具；不存在/超限拒绝，不误覆盖）。 */
  appendFile?: (path: string, content: string) => Promise<{ ok: boolean; summary: string }>;
  /** 整单替换写入当前会话/画布对话的任务清单（todo_write 工具；内部存 .atelyx/todos/，隐藏屏蔽的刻意豁免）。 */
  writeTodos?: (todos: TodoItem[]) => Promise<void>;
  /** 抓取网页正文（truncated = 正文是否命中大小上限被截断）。 */
  fetchUrl?: (url: string) => Promise<{ url: string; title?: string; content: string; truncated?: boolean }>;
}

/** 工具执行上下文（signal 中止 + 注入的能力）。 */
export interface ToolExecContext {
  signal: AbortSignal;
  capabilities: ToolCapabilities;
}

/**
 * 插件贡献的 AI 工具（`ctx.ai.registerTool` 载荷）：插件只给模型可见的 schema 与执行体，
 * 参数校验/气泡摘要/失败收敛由宿主补齐（插件入口自包含，不接触宿主内部工具契约）。
 */
export interface PluginToolOptions {
  /** 工具名（发给模型，全局唯一；小写字母/数字/下划线）。 */
  name: string;
  /** 发给模型的用途说明（模型据此决定是否调用）。 */
  description: string;
  /** 参数 JSON Schema；缺省 = 无参对象 schema。 */
  parameters?: Record<string, unknown>;
  /** 执行体：入参 = 模型给出的参数对象，返回气泡摘要文本；抛错即失败结果（不中断整轮）。
   *  第二参给中止信号（用户中止时置位），长任务请自行检查并尽快返回。 */
  run: (
    args: Record<string, unknown>,
    ctx: { signal: AbortSignal },
  ) => Promise<string> | string;
}

/** 自包含工具模块（defineTool 产出）。 */
export interface ToolDefinition<A = Record<string, unknown>> {
  /** 工具名（发给模型，也是注册表的联结 id）。 */
  name: string;
  description: string;
  /** 发给模型的 JSON Schema。 */
  parameters: Record<string, unknown>;
  /** 参数解析 + 校验（非法抛 `ToolArgsError`），返回强类型参数。 */
  validate: (args: unknown) => A;
  /** 气泡摘要。 */
  summarize: (args: A) => string;
  /** 执行（走注入的 capabilities）。 */
  execute: (args: A, exec: ToolExecContext) => Promise<ToolResult>;
  /**
   * 是否可与同轮其他工具**并行**执行（缺省 false = 串行屏障，等前一批收敛再跑）。
   * 只读/无副作用工具（read_file/glob/grep/list_dir/web_search/web_fetch）标 true；读写工具不标，防写竞态。
   */
  parallelSafe?: boolean;
  /** 回填模型的 tool 消息文本，缺省 = `result.content ?? result.summary`。 */
  renderResult?: (result: ToolResult) => string;
}

/** 工具注册表中「未知工具」的错误消息文本。 */
export const UNKNOWN_TOOL_MSG_PREFIX = "未知工具：";

/** 工具参数校验失败（由各工具的 validate 抛出）。 */
export class ToolArgsError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ToolArgsError";
  }
}

/** 从 catch 到的未知抛出值提取可读消息（工具层错误摘要统一走此收敛）。 */
export function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** 单个工具执行结果的可视化摘要（消息气泡工具块）。 */
export interface ToolExecResult {
  id: string;
  ok: boolean;
  summary: string;
  /** 完整结果文本（展开详情用；缺省 = summary）。 */
  detail?: string;
}
