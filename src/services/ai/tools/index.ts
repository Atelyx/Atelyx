/**
 * AI 工具（Agent 模式）执行层入口。
 *
 * - `AGENT_TOOLS`：注册表（自包含工具 + 按名分发），取代旧工具的 switch 分发。
 * - `buildAgentTools`：按勾选 id + 搜索是否已配置，组装发给模型的 `ToolSchema[]`。
 * - `runAgentTools`：执行一轮工具（差异由调用方 hooks 消化，如画布建产物节点）。
 * 工具只经注入的 `ToolCapabilities` 访问仓库/搜索能力，不 import store —— 可移植、可复用。
 */
import type {
  LlmMessage,
  LlmToolCall,
  PluginToolOptions,
  ToolDefinition,
  ToolExecContext,
  ToolExecResult,
  ToolResult,
  ToolSchema,
} from "@/types";
import { WEB_SEARCH_TOOL } from "./webSearch";
import { WEB_FETCH_TOOL } from "./webFetch";
import { READ_FILE_TOOL } from "./readFile";
import { READ_HISTORY_TOOL } from "./readHistory";
import { GLOB_TOOL } from "./glob";
import { GREP_TOOL } from "./grep";
import { LIST_DIR_TOOL } from "./listDir";
import { EDIT_FILE_TOOL } from "./editFile";
import { APPEND_FILE_TOOL } from "./appendFile";
import { WRITE_FILE_TOOL } from "./writeFile";
import { RENAME_FILE_TOOL } from "./renameFile";
import { MOVE_FILE_TOOL } from "./moveFile";
import { DELETE_FILE_TOOL } from "./deleteFile";
import { DELETE_DIR_TOOL } from "./deleteDir";
import { TODO_WRITE_TOOL } from "./todoWrite";
import { createToolRegistry } from "./registry";
import { errText } from "@/types";
import { FILE_REFERENCE_PROMPT, AGENT_TOOLS_META, type AgentToolMeta } from "@/constants/tools";

/** Agent 模式全部工具（注册顺序 = 名册/浮层展示顺序，与 AGENT_TOOLS_META 一致）。各工具参数类型各异，注册为通用定义。 */
let agentTools = [
  WEB_SEARCH_TOOL,
  WEB_FETCH_TOOL,
  READ_FILE_TOOL,
  GLOB_TOOL,
  GREP_TOOL,
  LIST_DIR_TOOL,
  READ_HISTORY_TOOL,
  EDIT_FILE_TOOL,
  APPEND_FILE_TOOL,
  WRITE_FILE_TOOL,
  RENAME_FILE_TOOL,
  MOVE_FILE_TOOL,
  DELETE_FILE_TOOL,
  DELETE_DIR_TOOL,
  TODO_WRITE_TOOL,
] as unknown as ToolDefinition[];

let registry = createToolRegistry(agentTools);

/** 插件注册的工具定义（对象同一性用于注销比对；UI 元数据据此派生）。 */
const pluginToolDefs: ToolDefinition[] = [];

/** 插件贡献工具：追加进注册表并重建分发器（运行时注册；对象同一性用于注销比对）。 */
export function registerPluginTools(defs: ToolDefinition[]): void {
  if (defs.length === 0) return;
  pluginToolDefs.push(...defs);
  agentTools = [...agentTools, ...defs];
  registry = createToolRegistry(agentTools);
}

/** 撤销插件贡献工具（卸载/停用时按对象同一性移除并重建分发器）。 */
export function unregisterPluginTools(defs: ToolDefinition[]): void {
  if (defs.length === 0) return;
  const drop = new Set(defs);
  const remaining = pluginToolDefs.filter((t) => !drop.has(t));
  pluginToolDefs.length = 0;
  pluginToolDefs.push(...remaining);
  agentTools = agentTools.filter((t) => !drop.has(t));
  registry = createToolRegistry(agentTools);
}

/** 插件工具的 UI 元数据（归入「插件」分类；label = 工具名，Agent 设置页据此展示勾选）。 */
export function pluginToolMetas(): AgentToolMeta[] {
  return pluginToolDefs.map((t) => ({ id: t.name, label: t.name, category: "plugin" }));
}

/** 工具名是否已被占用（内置或已注册的插件工具）：名字是注册表与模型名册的联结键，必须先判重。 */
export function isToolNameTaken(name: string): boolean {
  return agentTools.some((t) => t.name === name);
}

/** 插件工具载荷 → 自包含工具定义：schema/执行体来自插件，参数校验/摘要/失败收敛由宿主补齐。 */
export function pluginToolDefinition(opts: PluginToolOptions): ToolDefinition {
  return {
    name: opts.name,
    description: opts.description,
    parameters: opts.parameters ?? { type: "object", properties: {} },
    validate: (args) => (typeof args === "object" && args !== null ? (args as Record<string, unknown>) : {}),
    summarize: () => opts.name,
    execute: async (args, exec) => {
      try {
        // 中止信号透传给插件（长任务请自行响应；不响应则跑完，与内置工具的语义差由插件自负）。
        return { ok: true, summary: await opts.run(args, { signal: exec.signal }) };
      } catch (e) {
        return { ok: false, summary: `插件工具失败：${errText(e)}` };
      }
    },
  };
}

/** 运行前摘要（消息气泡工具块展示，容忍残缺参数）。 */
export function summarizeAgentTool(name: string, argsJson: string): string {
  return registry.summarize(name, argsJson);
}

/** 参数生成中宽松提取的字符串字段（JSON 仍残缺时取首个命中的值拼摘要；字段名与注册工具参数对齐）。 */
const PARTIAL_ARG_FIELD_RE = /"(?:path|query|pattern|url|dir|oldPath|newName|newDir)"\s*:\s*"((?:[^"\\]|\\.)*)/;

/**
 * 参数生成中的工具摘要（参数分片边到边刷新，供「生成中」工具行展示进度）：
 * 参数 JSON 尚不完整、正式摘要不可用——用工具显示名 + 宽松提取的首个关键字段值拼摘要，
 * 末尾附已生成长度（≥10000 显示为 x.xk），让长参数（如 write_file 正文）的生成过程可见。
 */
export function summarizePartialAgentTool(name: string, partialJson: string): string {
  const label = AGENT_TOOLS_META.find((t) => t.id === name)?.label ?? name;
  const hit = PARTIAL_ARG_FIELD_RE.exec(partialJson);
  const len = partialJson.length;
  const size = len >= 10000 ? `${(len / 1000).toFixed(1)}k` : `${len}`;
  const progress = len > 0 ? `（生成中 ${size} 字符）` : "";
  return hit ? `${label} ${hit[1]}${progress}` : `${label}${progress}`;
}

/** 组装结果：发给模型的工具名册 + 本次剔除提示。 */
export interface AgentToolAssembly {
  tools: ToolSchema[];
  /** web_search 被勾选但搜索源未配置（调用方据此提示）。 */
  skippedWebSearch: boolean;
}

/** 按勾选 id + 搜索配置组装工具名册；未知 id 静默忽略（不在名册即不并入）。
 * 全部工具均以 enabledIds 为准——勾选即赋予、取消即移除；
 * web_search 未配置搜索源时剔除（勾选状态下 skippedWebSearch 置 true 供调用方提示）。 */
export function buildAgentTools(
  enabledIds: string[],
  searchReady: boolean,
): AgentToolAssembly {
  const tools: ToolSchema[] = [];
  let skippedWebSearch = false;
  for (const def of agentTools) {
    if (!enabledIds.includes(def.name)) continue;
    if (def.name === "web_search" && !searchReady) {
      skippedWebSearch = true;
      continue;
    }
    tools.push({ name: def.name, description: def.description, parameters: def.parameters });
  }
  return { tools, skippedWebSearch };
}

/**
 * 组装发送给模型的 system 消息文本：Agent 系统提示词 + 引用文件读取引导。
 * 工具含 read_file 时追加引导（read_file 是否在名册由 Agent 勾选决定，勾选时才注入）——让模型知道 @引用 的笔记
 * 应经 read_file 按路径读取正文，而不是假装看过内容。两 store（画布/面板）共用防行为分叉。
 * 注意：易变上下文（当前任务清单/当前笔记）一律走**尾部 user 消息块**注入（见
 * agentTodos.currentTodosBlock / chatPanelStore.currentNoteContextBlock），不进系统提示词——
 * 系统前缀必须保持稳定以命中前缀缓存。
 */
export function assembleAgentSystemPrompt(
  systemPrompt: string | undefined,
  tools: ToolSchema[],
): string | undefined {
  const parts: string[] = [];
  if (systemPrompt?.trim()) parts.push(systemPrompt);
  if (tools.some((t) => t.name === "read_file")) parts.push(FILE_REFERENCE_PROMPT);
  return parts.length ? parts.join("\n\n") : undefined;
}

/** 工具产物 hooks（画布建节点 vs 面板不建 的差异收敛于此）。 */
export interface AgentToolHooks {
  /** 工具成功产物回调（data 为具体 payload：write_file → {path}）。 */
  onToolResult?: (name: string, result: ToolResult) => void;
}

/** 执行一轮工具调用（公共执行器，画布/面板共用）；并行在飞上限取 `MAX_PARALLEL_TOOL_CALLS` 常量。 */
export async function runAgentTools(
  calls: LlmToolCall[],
  exec: ToolExecContext,
  hooks?: AgentToolHooks,
): Promise<{ messages: LlmMessage[]; results: ToolExecResult[] }> {
  const { messages, results, outcomes } = await registry.dispatch(calls, exec);
  if (hooks?.onToolResult) {
    for (const o of outcomes) {
      try {
        hooks.onToolResult(o.name, o.result);
      } catch {
        // 产物节点创建失败只跳过该产物，不回断整轮结果回填（否则 done 不回调、工具行永久转圈）
      }
    }
  }
  return { messages, results };
}
