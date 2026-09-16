/**
 * AI 对话核心能力契约（核心运行时 ↔ 消费方）。
 *
 * 核心只跑「一轮对话」——解析目标、组装提示词与工具名册、流式生成、执行工具、收尾判定、
 * 话题命名；**不持有消息容器、不落盘、不感知视图**。消息存哪、怎么落盘、何时广播由消费方决定：
 * 核心把产出经 `ChatTurnSink` 交回，消费方在自己的写入器里一并处理存在性守卫、协作与落盘调度。
 * 类型全部中性（不含会话/节点概念），面板、画布对话节点与插件消费同一套编排。
 */
import type {
  AgentStep,
  Attachment,
  ChatTargetResult,
  ConversationCompaction,
  ProviderConfig,
  ReasoningEffort,
  Role,
  ToolCapabilities,
  ToolResult,
} from "@/types";

/** 对话目标覆盖（面板/节点级模型选择；null 或字段缺省 = 跟随仓库默认模型）。 */
export interface ChatTargetSelection {
  providerId?: string;
  model?: string;
}

/** 已解析的对话目标（消费方先经 `ChatRuntime.resolveTarget` 解析；解析失败不进本轮，由消费方提示）。 */
export interface ChatTurnTarget {
  provider: ProviderConfig;
  model: string;
}

/** 一轮对话的中性消息（消费方容器 → 核心）。 */
export interface ChatTurnMessage {
  /** 稳定 id（压缩注解锚点按它定位；消费方容器的消息 id）。 */
  id: string;
  role: Role;
  content: string;
  /** 气泡展示用原始输入（@引用 展开前的文本）；话题命名摘要按它取正文。 */
  displayContent?: string;
  /** 工具步进（思考/叙述/工具调用；重建请求历史时展开为线上工具序列）。 */
  steps?: AgentStep[];
  /** user 消息附件（图片/文本）；内容不内嵌，消费方须先按引用补齐 payload。 */
  attachments?: Attachment[];
}

/** 工具能力覆盖工厂：入参 = 核心的标准能力集（已绑定目标），返回要覆盖的方法（同名覆盖，其余用标准）。 */
export type ChatCapabilityOverrides = (standard: ToolCapabilities) => Partial<ToolCapabilities>;

/** 消费方钩子：核心标准行为之外的分支（缺省 = 标准行为）。 */
export interface ChatTurnHooks {
  /** 每条工具结果回调（画布据此建搜索/写笔记产物节点）。 */
  onToolResult?: (name: string, result: ToolResult) => void;
  /** 覆盖标准工具能力（如把 read_file 包一层做引用物质化）。 */
  capabilities?: ChatCapabilityOverrides;
}

/**
 * 消息写入器：核心把一轮对话的产出交回消费方容器。
 * 容器形状各异（面板会话数组 / 画布节点消息表 / 插件自有容器），故接缝只暴露「写什么」、
 * 不暴露「写到哪」——存在性守卫（会话/节点已删）、协作与落盘调度由消费方在实现里处理。
 */
export interface ChatTurnSink {
  /** 流式中间态：正文与步骤的当前快照（引擎每帧合并后调用，步骤组装已含思考/叙述/工具过程）。 */
  update(state: { content: string; steps: AgentStep[] }): void;
  /** 一轮结束：正文与步骤已 finalize（叙述提升/截断提示），消费方据此写回或移除占位。 */
  finish(result: ChatTurnOutcome): void;
  /** 请求失败：消费方写错误占位（保留已产出内容）并复位流式态。 */
  fail(error: Error): void;
  /** 非致命提示（如搜索源未配置、本次未启用联网搜索）：消费方自行展示，不中断本轮。 */
  notice(message: string): void;
}

/** 一轮对话的收尾结果（空回复/超时/截断判定已在核心做完，消费方只执行）。 */
export interface ChatTurnOutcome {
  /** 最终正文（已含叙述提升与截断提示）；removed = true 时为错误占位文案。 */
  content: string;
  /** 最终步骤（含未提升的思考/工具步）。 */
  steps: AgentStep[];
  /** 空回复（无正文、无步骤、非超时）：消费方移除占位消息。 */
  removed: boolean;
  /** 空闲超时（已有产出时为提示；无产出时 content 为超时错误文案）。 */
  timedOut: boolean;
  /** 被中止（用户停止/切换仓库/删除容器）。 */
  aborted: boolean;
}

/** 话题命名目标：核心按此在轮末命名（读消息/是否已命名/写回标题由消费方提供）。 */
export interface ChatNamingTarget {
  /** 重取当前消息列表（延迟后调用；目标已消失返回空数组）。 */
  getMessages(): Array<{ role: Role; content: string; displayContent?: string }>;
  /** 是否已被命名（画布 = 节点 title 非空；面板 = 已登记成功命名）。 */
  isNamed(): boolean;
  /** 写回标题（成功后调用）。 */
  applyTitle(title: string): void;
}

/** 话题命名结果：ok = 成功写回；skipped = 无模型/无消息/已命名/被中止（调用方静默）；failed = 请求失败（可重试）。 */
export type ChatAutoNameResult = "ok" | "skipped" | "failed";

/** 话题命名选项。 */
export interface ChatAutoNameOptions {
  /** 延迟毫秒（缺省 3s 防限流；用户显式重新命名传 0）。 */
  delayMs?: number;
  /** 摘要字符上限（缺省见常量；重新命名传 Infinity）。 */
  maxChars?: number;
  /** 忽略「话题自动命名」开关（用户显式请求）。 */
  ignoreToggle?: boolean;
}

/** 一轮对话请求。 */
export interface ChatTurnRequest {
  /** 目标标识（面板会话 id / 画布对话节点 id / 插件自定 id）：任务清单侧车与命名中止按它键控。 */
  targetId: string;
  /** 已解析的对话目标（消费方先经 resolveTarget 解析）。 */
  target: ChatTurnTarget;
  /** 容器历史（含本轮新追加的 user 消息）。 */
  history: ChatTurnMessage[];
  /** 推理等级覆盖（与模型覆盖正交；缺省 = 不下发 reasoning_effort）。 */
  reasoningEffort?: ReasoningEffort;
  /** Agent 配置 id（系统提示词与工具名册来源；缺省 = 预置「对话」Agent）。 */
  agentId?: string;
  /** 压缩注解（锚点及其之前的消息不进请求，由摘要代替）。 */
  compaction?: ConversationCompaction;
  /** 把当前打开的笔记以尾部上下文块注入（需 read_file 在名册内；面板开启、画布不开）。 */
  includeCurrentNote?: boolean;
  /** 中止信号（句柄由消费方持有：画布多节点并发互不干扰）。 */
  signal: AbortSignal;
  sink: ChatTurnSink;
  naming: ChatNamingTarget;
  hooks?: ChatTurnHooks;
}

/** 压缩请求：把当前模型可见历史（旧摘要 + 未被旧注解覆盖的保留段）截到新边界重新总结。 */
export interface ChatCompactRequest {
  /** 已解析的对话目标（消费方先经 resolveTarget 解析）。 */
  target: ChatTurnTarget;
  /** 容器完整消息（含最新一轮）。 */
  messages: ChatTurnMessage[];
  /** 现有压缩注解（旧摘要随新内容一并重新总结，压缩两次不丢先前摘要）。 */
  compaction?: ConversationCompaction;
  /** 新边界锚点（消费方用 nextCompactionBoundary 求得）。 */
  upToMessageId: string;
  /** Agent 配置 id（工具名册与最近一次真实请求结构一致）。 */
  agentId?: string;
  signal: AbortSignal;
}

/** 压缩结果：成功给摘要与产出模型（消费方写入自己的压缩注解）；失败给可展示文案（aborted = 用户中止，调用方静默）。 */
export type ChatCompactResult =
  | { ok: true; summary: string; providerId: string; model: string }
  | { ok: false; aborted: boolean; message: string };

/** AI 对话核心能力面（同仓消费方经注册表取用，插件经 `ctx.chat` 取用，是同一对象）。 */
export interface ChatRuntime {
  /** 解析对话目标（未指定 = 跟随仓库默认；失败给可展示文案，调用方负责提示）。 */
  resolveTarget(selection?: ChatTargetSelection | null): ChatTargetResult;
  /** 跑一轮对话（流式 + 工具循环 + 收尾 + 命名），产出经 sink 交回消费方。 */
  runTurn(req: ChatTurnRequest): Promise<void>;
  /** 生成压缩摘要（只出文本，写回容器由消费方负责）。 */
  compact(req: ChatCompactRequest): Promise<ChatCompactResult>;
  /** 话题命名（轮末自动命名与手动重新命名共用）。 */
  autoName(
    naming: ChatNamingTarget,
    targetId: string,
    opts?: ChatAutoNameOptions,
  ): Promise<ChatAutoNameResult>;
}
