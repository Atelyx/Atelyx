/**
 * OpenAI 兼容协议的适配器（当前唯一供应商适配器，契约提供者中立）。
 * 前端直连，SSE 流式输出。
 *
 * - `streamRequest`：中性 `LlmMessage[]` → 发起一次请求，异步产出中性 `LlmStreamEvent`（单次尝试，不含重试）。
 * - `streamChat`：消费 `streamRequest`，叠加传输级重试策略（retry.ts），对外保留回调 API，供流式引擎调用。
 * - `chatOnce`：非流式单次（标题生成等一次性任务）。
 * - `messagesToWire` / `toLlmMessages`：中性词 ⇄ 内部消息/线协议的纯转换。
 *
 * 加新供应商 = 按同一中性接缝再实现一个适配器，调用方无感知。
 * key 由用户在设置中填入，本地加密存储，运行时解密到内存。
 */
import type {
  Attachment,
  Role,
  ReasoningEffort,
  ToolSchema,
  LlmFinishReason,
  LlmMessage,
  LlmStreamEvent,
  LlmToolCall,
  LlmToolCallDelta,
} from "@/types";
import { withOverflowHint, toLlmError, isRetryableError, LlmError } from "./errors";
import { computeRetryDelay, GIVE_UP_RETRY_MS, shouldRetry, sleep } from "./retry";

/** 流式空闲超时：SSE 长时间无新 token 视为挂起（调用方据此自动中止降级，见 streaming.ts）。 */
export const STREAM_IDLE_TIMEOUT_MS = 60_000;

/** 连通性测试/模型列表拉取超时：黑盒端点可能只收不答（代理吞包/黑洞 IP），防设置页按钮无限转圈。 */
const FETCH_MODELS_TIMEOUT_MS = 15_000;

export interface ChatStreamCallbacks {
  onDelta: (text: string) => void;
  /** 流结束（含超时/中止）。reason = 服务端 finish_reason 归一化后的 LlmFinishReason。 */
  onDone: (reason?: LlmFinishReason) => void;
  onError: (err: Error) => void;
  /** 模型思考过程增量（`delta.reasoning_content`，思考型模型的推理阶段内容）。 */
  onReasoningDelta?: (text: string) => void;
  /** 工具调用参数增量（边生成边发）：供调用方喂空闲超时看门狗并实时展示「生成参数中」的工具行。 */
  onToolCallDelta?: (delta: LlmToolCallDelta) => void;
  /** 响应含工具调用时触发（在 onDone 之前）。calls 已按 index 累积完整。 */
  onToolCalls?: (calls: LlmToolCall[]) => void;
}

export interface ChatParams {
  baseUrl: string;
  apiKey: string;
  model: string;
  messages: LlmMessage[];
  /** 采样温度；不传 = 请求体不含该字段，使用各厂商 API 默认配置。 */
  temperature?: number;
  /** 思考档位：下发 `reasoning_effort` 以开启模型思考；缺省/`off` = 请求体不含（跟随供应商/模型默认）。 */
  reasoningEffort?: ReasoningEffort;
  /** 单次响应最大 token 数。仅短任务（如标题生成）显式传入；主对话不传（不截断回复）。 */
  maxTokens?: number;
  signal?: AbortSignal;
  tools?: ToolSchema[];
  /** 传输级重试：仅在「请求未建立或未收到任何 SSE 事件」的失败上重试；流开始后不重试；abort 后永不重试。缺省 = 不重试。 */
  retry?: {
    maxRetries?: number;
    onRetry?: (attempt: number, delayMs: number) => void;
  };
}

/** 中性工具名册 → OpenAI 线上 function 定义。 */
function toolsToWire(tools: ToolSchema[]): Array<{
  type: "function";
  function: { name: string; description: string; parameters: Record<string, unknown> };
}> {
  return tools.map((t) => ({
    type: "function",
    function: { name: t.name, description: t.description, parameters: t.parameters },
  }));
}

/** 中性工具调用 → OpenAI 线上 tool_calls 形状（type 恒为 function，llama.cpp 解析必须带该字段）。 */
function toolCallsToWire(toolCalls: LlmToolCall[]) {
  return toolCalls.map((c) => ({
    id: c.id,
    type: "function",
    function: { name: c.name, arguments: c.arguments },
  }));
}

/** user 消息 → content parts 数组（文本 + 文件文本 + 图片）。 */
function userContentParts(m: Extract<LlmMessage, { role: "user" }>) {
  const parts: Array<{ type: string; text?: string; image_url?: { url: string } }> = [];
  if (m.text) parts.push({ type: "text", text: m.text });
  for (const t of m.fileTexts ?? []) parts.push({ type: "text", text: t });
  for (const img of m.images ?? []) parts.push({ type: "image_url", image_url: { url: img.url } });
  return parts;
}

/** 中性 LlmMessage[] → OpenAI 线上 messages（适配器私有，供应商差异隔离于此）。 */
function messagesToWire(
  messages: LlmMessage[],
  system?: string,
): Array<Record<string, unknown>> {
  const wire: Array<Record<string, unknown>> = [];
  if (system) wire.push({ role: "system", content: system });
  for (const m of messages) {
    switch (m.role) {
      case "system":
        wire.push({ role: "system", content: m.text });
        break;
      case "user":
        wire.push({ role: "user", content: userContentParts(m) });
        break;
      case "assistant":
        wire.push({
          role: "assistant",
          // assistant 带 tool_calls 时 content 为 null（OpenAI 规范，部分兼容网关对空串返回 500）
          content: m.text,
          ...(m.toolCalls?.length ? { tool_calls: toolCallsToWire(m.toolCalls) } : {}),
        });
        break;
      case "tool":
        wire.push({ role: "tool", content: m.text ?? "", tool_call_id: m.toolCallId });
        break;
    }
  }
  return wire;
}

/**
 * 将内部消息（画布 `Message[]` / 对话面板 `EditorChatMessage[]`）转为中性 `LlmMessage[]`。
 * 纯数据转换，不做 I/O。文本/搜索引用在 send 时已拼进 user message content；图片附件 → images，文件附件 → fileTexts。
 * 附件内容不随消息内嵌，调用方须先按引用补齐（见各 store 的附件补齐步骤）。
 */
export function toLlmMessages(
  messages: Array<{ role: Role; content: string; attachments?: Attachment[] }>,
): LlmMessage[] {
  return messages.map((m): LlmMessage => {
    if (m.role === "user") {
      if (m.attachments?.length) {
        const images: { url: string }[] = [];
        const fileTexts: string[] = [];
        for (const a of m.attachments) {
          // 内容随消息不内嵌：payload 由调用方按引用补齐后进来。两类都留空守卫——
          // 空 dataURL 会产出 `image_url:{}`，真实端点按整条请求 400，单张图读失败不该拖垮整轮对话。
          if (!a.payload) continue;
          if (a.kind === "image") images.push({ url: a.payload });
          else fileTexts.push(a.payload);
        }
        return {
          role: "user",
          text: m.content,
          ...(images.length ? { images } : {}),
          ...(fileTexts.length ? { fileTexts } : {}),
        };
      }
      return { role: "user", text: m.content };
    }
    // system / assistant（tool 由引擎直接构造；此处调用方只传 internal role）
    return { role: m.role, text: m.content };
  });
}

/**
 * 补齐消息附件内容并返回补齐后的消息数组：只处理「有 `file` 引用且内容缺失」的附件。
 * 附件字节不随消息内嵌（画布/会话只存引用），发送前必须按引用读回；其余消息原样返回，
 * 已补齐的附件不重复读盘。返回新数组但复用未变消息引用——调用方据此判「是否需要回写缓存」。
 */
export async function resolveMessageAttachments<T extends MessageLike>(
  messages: T[],
  readAttachment: (att: Attachment) => Promise<string>,
): Promise<T[]> {
  let changed = false;
  const out = await Promise.all(
    messages.map(async (m) => {
      if (m.role !== "user" || !m.attachments?.length) return m;
      const missing = m.attachments.filter((a) => a.file && !a.payload);
      if (missing.length === 0) return m;
      const attachments = await Promise.all(
        m.attachments.map(async (a) =>
          a.file && !a.payload ? { ...a, payload: await readAttachment(a) } : a,
        ),
      );
      changed = true;
      return { ...m, attachments };
    }),
  );
  return changed ? out : messages;
}

/** `resolveMessageAttachments` 的输入形状（画布 Message / 面板会话消息同构）。 */
interface MessageLike {
  role: Role;
  content: string;
  attachments?: Attachment[];
}

/* ------------------------------------------------------------------ */

interface StreamRequest {
  url: string;
  apiKey: string;
  model: string;
  messages: LlmMessage[];
  temperature?: number;
  reasoningEffort?: ReasoningEffort;
  maxTokens?: number;
  signal?: AbortSignal;
  tools?: ToolSchema[];
}

interface ToolCallDelta {
  index: number;
  id?: string;
  function?: { name?: string; arguments?: string };
}

/**
 * 发起一次流式请求，异步产出中性 `LlmStreamEvent`（单次尝试，不含重试）。
 * 传输/HTTP 失败在未产出任何事件前以 `LlmError` 抛出；abort 抛 AbortError。
 */
export async function* streamRequest(
  req: StreamRequest,
): AsyncGenerator<LlmStreamEvent> {
  const { url, apiKey, model, messages, temperature, reasoningEffort, maxTokens, signal, tools } = req;
  // 是否产出过有效事件：供 streamChat 判断「流开始后」不再重试
  let receivedAnyEvent = false;

  try {
    const res = await fetch(url, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        // 自建网关无内置 SSE 时依赖此头决定以流式返回
        Accept: "text/event-stream",
        Authorization: `Bearer ${apiKey}`,
      },
      body: JSON.stringify({
        model,
        messages: messagesToWire(messages),
        // 不传 temperature = 使用各厂商 API 默认配置
        temperature,
        ...(!reasoningEffort || reasoningEffort === "off"
          ? {}
          : { reasoning_effort: reasoningEffort }),
        ...(maxTokens !== undefined ? { max_tokens: maxTokens } : {}),
        stream: true,
        ...(tools?.length ? { tools: toolsToWire(tools) } : {}),
      }),
      signal,
    });

    if (!res.ok || !res.body) {
      const bodyText = await res.text().catch(() => "");
      const err = toLlmError(`HTTP ${res.status} (${url} | model: ${model}): ${bodyText}`, {
        status: res.status,
        retryAfterMs: computeRetryDelay(0, res),
      });
      throw err;
    }

    const reader = res.body.getReader();
    const decoder = new TextDecoder("utf-8");
    let buffer = "";
    let stopReason: string | undefined;
    const toolCallAcc: ToolCallDelta[] = [];

    const emitToolCalls = function* () {
      for (const t of toolCallAcc) {
        yield {
          type: "tool-call",
          call: {
            id: t.id ?? "",
            name: t.function?.name ?? "",
            arguments: t.function?.arguments ?? "",
          },
        } satisfies LlmStreamEvent;
      }
    };

    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });

      // SSE 以空行分隔事件（兼容 \r\n\r\n 与 \n\n）
      const parts = buffer.split(/\r?\n\r?\n/);
      buffer = parts.pop() ?? "";

      for (const part of parts) {
        const line = part.trim();
        if (!line.startsWith("data:")) continue;
        const data = line.slice(5).trim();
        if (data === "[DONE]") {
          yield* emitToolCalls();
          yield { type: "finish", reason: mapStopReason(stopReason) };
          return;
        }
        try {
          const json = JSON.parse(data);
          // 部分兼容网关流式中途出错会发 error 事件（HTTP 仍是 200）——如实上报，不静默跳过
          if (json.error) {
            const errMsg = json.error as { message?: string } | string;
            const msg = typeof errMsg === "string" ? errMsg : (errMsg.message ?? JSON.stringify(json.error));
            throw new Error(`SSE 错误事件: ${msg}`);
          }
          receivedAnyEvent = true;
          const choice = json.choices?.[0];
          if (choice?.finish_reason) stopReason = choice.finish_reason;
          const delta = choice?.delta;
          if (!delta) continue;
          if (delta.content) yield { type: "text-delta", text: delta.content };
          // 思考过程增量：多字段探测（思考型模型在推理阶段只发思考字段，各家命名不一）；
          // 仅接受非空字符串——个别兼容网关把该字段发成对象/空串，混入会导致思考文本被污染
          const reasoning = delta.reasoning_content ?? delta.reasoning ?? delta.reasoning_text;
          if (typeof reasoning === "string" && reasoning.length > 0) {
            yield { type: "reasoning-delta", text: reasoning };
          }
          // 工具调用：{ index, id?, function: { name?, arguments? } }，按 index 累积；
          // 分片同时以 tool-call-delta 边生成边发（参数进度实时可见），完整调用在流末 emitToolCalls 一次性发出
          const tc = delta.tool_calls as ToolCallDelta[] | undefined;
          if (tc) {
            for (const t of tc) {
              const slot = toolCallAcc[t.index];
              if (slot) {
                if (t.id) slot.id = t.id;
                if (t.function?.name) slot.function = { ...slot.function, name: t.function.name };
                if (t.function?.arguments) {
                  slot.function = { ...slot.function, arguments: (slot.function?.arguments ?? "") + t.function.arguments };
                }
              } else {
                toolCallAcc[t.index] = {
                  index: t.index,
                  id: t.id,
                  function: t.function ? { name: t.function.name, arguments: t.function.arguments ?? "" } : undefined,
                };
              }
              yield {
                type: "tool-call-delta",
                index: t.index,
                ...(t.id ? { id: t.id } : {}),
                ...(t.function?.name ? { name: t.function.name } : {}),
                argumentsDelta: t.function?.arguments ?? "",
              };
            }
          }
        } catch (err) {
          if (err instanceof Error && err.message.startsWith("SSE 错误事件")) throw err;
          // 单帧解析失败跳过，不中断流
        }
      }
    }

    yield* emitToolCalls();
    if (!receivedAnyEvent && buffer.trim()) {
      // 网关忽略 stream:true 直接返回了完整 JSON：按非流式兜底解析一次，避免「发了消息但毫无反应」
      try {
        const json = JSON.parse(buffer.trim());
        const text: unknown = json?.choices?.[0]?.message?.content;
        if (typeof text === "string" && text) {
          yield { type: "text-delta", text };
        }
      } catch {
        // 不是 JSON：落报错分支
      }
    }
    yield { type: "finish", reason: mapStopReason(stopReason) };
  } catch (err) {
    if ((err as Error).name === "AbortError") throw err;
    // 溢出提示统一由 streamChat 的 onError 出口包裹一次（此处不包，防双重）
    throw err;
  }
}

/** 服务端 finish_reason 归一化为中性停止原因。 */
function mapStopReason(finishReason?: string): LlmFinishReason {
  if (finishReason === "tool_calls") return "tool-calls";
  if (finishReason === "length") return "max-tokens";
  return "stop";
}

/**
 * 发起流式聊天请求（消费 streamRequest + 重试策略），对外保留回调 API。
 * 失败降级：可重试的传输级失败（网络/5xx/429）按指数退避（尊重 retry-after），其余走 onError；
 * abort 后永不重试、按 onDone 收敛。
 */
export async function streamChat(
  params: ChatParams,
  callbacks: ChatStreamCallbacks,
): Promise<void> {
  const { baseUrl, apiKey, model, messages, temperature, reasoningEffort, maxTokens, signal, tools, retry } = params;
  const url = `${baseUrl.replace(/\/$/, "")}/chat/completions`;
  const maxRetries = retry?.maxRetries ?? 0;

  for (let attempt = 0; ; attempt++) {
    let receivedAnyEvent = false;
    try {
      const toolCalls: LlmToolCall[] = [];
      let finished = false;
      for await (const event of streamRequest({
        url,
        apiKey,
        model,
        messages,
        temperature,
        reasoningEffort,
        maxTokens,
        signal,
        tools,
      })) {
        // 收到任意事件即视为「流已开始」：其后的中断不重试（防已累积的工具调用/已产出内容重复输出）
        receivedAnyEvent = true;
        switch (event.type) {
          case "text-delta":
            callbacks.onDelta(event.text);
            break;
          case "reasoning-delta":
            callbacks.onReasoningDelta?.(event.text);
            break;
          case "tool-call-delta":
            callbacks.onToolCallDelta?.(event);
            break;
          case "tool-call":
            toolCalls.push(event.call);
            break;
          case "finish":
            if (toolCalls.length) callbacks.onToolCalls?.(toolCalls);
            callbacks.onDone(event.reason);
            finished = true;
            break;
        }
      }
      if (!finished) {
        // 生成器被提前 return（不应发生，兜底收敛）
        if (toolCalls.length) callbacks.onToolCalls?.(toolCalls);
        callbacks.onDone();
      }
      return;
    } catch (err) {
      if ((err as Error).name === "AbortError") {
        callbacks.onDone();
        return;
      }
      const e = err as Error;
      if (shouldRetry(isRetryableError(e), attempt, maxRetries, receivedAnyEvent)) {
        const delayMs =
          e instanceof LlmError && e.retryAfterMs !== undefined
            ? e.retryAfterMs
            : computeRetryDelay(attempt, undefined);
        // 服务端要求等待超限（哨兵）→ 放弃重试直接判负，避免 <8s 退避连打长冷却服务器
        if (delayMs === GIVE_UP_RETRY_MS) {
          callbacks.onError(withOverflowHint(e));
          return;
        }
        retry?.onRetry?.(attempt + 1, delayMs);
        const waited = await sleep(delayMs, signal);
        if (!waited) {
          callbacks.onDone();
          return;
        }
        continue;
      }
      callbacks.onError(withOverflowHint(e));
      return;
    }
  }
}

/**
 * 发起非流式单次聊天请求，返回完整回复文本（标题生成等一次性任务）。
 * 边界捕获：网络/解析失败抛错，由调用方降级。
 */
export async function chatOnce(params: ChatParams): Promise<string> {
  const { baseUrl, apiKey, model, messages, temperature, maxTokens, signal } = params;
  const url = `${baseUrl.replace(/\/$/, "")}/chat/completions`;
  const res = await fetch(url, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${apiKey}`,
    },
    body: JSON.stringify({
      model,
      messages: messagesToWire(messages),
      temperature,
      ...(maxTokens !== undefined ? { max_tokens: maxTokens } : {}),
      stream: false,
    }),
    signal,
  });
  if (!res.ok) {
    const err = toLlmError(`HTTP ${res.status}: ${await res.text().catch(() => "")}`, {
      status: res.status,
    });
    throw withOverflowHint(err);
  }
  const json = await res.json();
  const text: unknown = json?.choices?.[0]?.message?.content;
  if (typeof text !== "string") {
    throw new Error("响应缺少 content");
  }
  return text;
}

/**
 * 拉取供应商可用模型列表（`GET {baseUrl}/models`，OpenAI 兼容标准端点）。
 * 同时用作设置页「获取模型列表」与「测试连通性」（端点可达 + key 有效，免计费）。
 * 边界捕获：网络/解析失败抛错（带 HTTP 状态与响应摘要），由调用方降级提示。
 */
export async function fetchProviderModels(
  baseUrl: string,
  apiKey: string,
): Promise<string[]> {
  const url = `${baseUrl.replace(/\/$/, "")}/models`;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), FETCH_MODELS_TIMEOUT_MS);
  try {
    const res = await fetch(url, {
      headers: { Authorization: `Bearer ${apiKey}` },
      signal: controller.signal,
    });
    if (!res.ok) {
      throw toLlmError(`HTTP ${res.status} (${url}): ${await res.text().catch(() => "")}`, {
        status: res.status,
      });
    }
    const json = (await res.json()) as { data?: Array<{ id?: string }> };
    const ids = (json.data ?? []).map((m) => m.id).filter((id): id is string => !!id);
    if (!ids.length) {
      throw new Error("响应中没有模型列表（data 为空）");
    }
    return ids;
  } catch (err) {
    if ((err as Error).name === "AbortError") {
      throw new Error("请求超时（15 秒），请检查 Base URL 与网络");
    }
    throw err;
  } finally {
    clearTimeout(timer);
  }
}
