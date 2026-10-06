/**
 * 提供者中立的 LLM 词汇。适配器负责「中性词汇 ⇄ 供应商线上格式」翻译，引擎/调用方只接触
 * 中性类型，不感知具体供应商线上协议。类型刻意收敛到 UI 所需（文本 + 思考），不引入完整
 * content-block 数组。
 */

/** 一次工具调用（中性的 function-call 描述）。 */
export interface LlmToolCall {
  id: string;
  name: string;
  /** 模型产出的原始参数 JSON 串。 */
  arguments: string;
}

/** 工具调用参数流式分片（线上 tool_calls 增量原样转发，参数边生成边发）。 */
export interface LlmToolCallDelta {
  /** 调用在本次响应内的序号（与线上 tool_calls index 对应）。 */
  index: number;
  id?: string;
  name?: string;
  argumentsDelta: string;
}

/** 用户消息的多模态部分（适配器折成 content parts：text / image）。 */
type UserParts = {
  /** 图片附件：data URL（vision）。 */
  images?: { url: string }[];
  /** 文件附件解析出的文本片段（与正文并列注入）。 */
  fileTexts?: string[];
};

/** 除 user 外的消息（system/assistant/tool）体。 */
type PlainLlmMessage = {
  role: "system" | "assistant" | "tool";
  text: string | null;
  /** assistant 带工具调用时 text 为 null（OpenAI 规范）。 */
  toolCalls?: LlmToolCall[];
  /** tool 消息回填时指向对应调用的 id。 */
  toolCallId?: string;
};

/** user 消息体（可带多模态附件）。 */
type UserLlmMessage = {
  role: "user";
  text: string;
} & UserParts;

/** 提供者中立的对话消息。 */
export type LlmMessage = PlainLlmMessage | UserLlmMessage;

/** 模型为何停止（收敛到我们需要的子集；用户停止/请求失败不产出 finish 事件，不走此枚举）。 */
export type LlmFinishReason =
  | "stop"
  | "tool-calls"
  | "max-tokens";

/**
 * 适配器原始流事件。
 * 工具调用参数分片以 tool-call-delta 边生成边发（供 UI 实时展示参数进度 + 空闲超时喂狗）；
 * 完整调用仍在流末以 tool-call 一次性发出，工具执行只认完整调用。
 */
export type LlmStreamEvent =
  | { type: "text-delta"; text: string }
  | { type: "reasoning-delta"; text: string }
  | { type: "tool-call-delta"; index: number; id?: string; name?: string; argumentsDelta: string }
  | { type: "tool-call"; call: LlmToolCall }
  | { type: "finish"; reason: LlmFinishReason };

