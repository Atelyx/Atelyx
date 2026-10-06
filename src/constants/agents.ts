/**
 * Agent 预置常量：默认随每个仓库出现的两个不可删除 Agent（builtin 标记）。
 * 可编辑（改名/改工具/配提示词）但不可删除；settingsStore 加载缺失时补入并落盘。
 */
import type { AgentConfig } from "@/types";
import { DEFAULT_AGENT_TOOLS, READONLY_TOOL_IDS } from "@/constants/tools";

/** 预置「对话」Agent 固定 id（不可删除）。 */
export const BUILTIN_AGENT_CHAT_ID = "builtin-chat";
/** 预置「Agent」Agent 固定 id（不可删除）。 */
const BUILTIN_AGENT_AGENT_ID = "builtin-agent";

/** 预置「对话」Agent 工具集：联网能力 + 文件读取能力（显式允许清单而非排除式过滤，防后续新增写入类工具漏进无写入语义的「对话」；
 * 文件读取能力显式列出，保证「对话」默认即可读 @引用 笔记；用户仍可在设置页取消。 */
const BUILTIN_CHAT_TOOLS: string[] = [
  "web_search",
  "web_fetch",
  ...READONLY_TOOL_IDS,
];

/** 预置 Agent 列表（顺序 = 列表/下拉展示顺序，恒置顶）。 */
export const BUILTIN_AGENTS: AgentConfig[] = [
  { id: BUILTIN_AGENT_CHAT_ID, name: "对话", tools: [...BUILTIN_CHAT_TOOLS], builtin: true },
  {
    id: BUILTIN_AGENT_AGENT_ID,
    name: "Agent",
    tools: [...DEFAULT_AGENT_TOOLS],
    builtin: true,
  },
];
