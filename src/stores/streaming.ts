/**
 * 公共流式对话引擎：画布对话节点（canvasStore.runStream）与 AI 对话面板（chatPanelStore.runExchange）共用。
 *
 * 一轮完整流式对话 = 无预算工具循环（模型不调工具即收束，靠 max-tokens 兜底 + 停止按钮打断）+
 * 双通道 rAF 节流 + 空闲超时 + SSE 流式。
 * 状态容器差异（messagesByConv vs sessions）由调用方回调消化：
 * - applyBatch：每帧合并后的增量写回调
 * - onError：请求失败写 [错误] 占位（保留已产出内容）
 * - onDone：流结束（含超时/中止/截断），调用方用 decideCleanup 做最终清理；`truncated` = 达到输出上限，
 *   `promoteNarration` = 最终回答轮（无工具调用）其叙述行应在轮末提升进 content
 * - executeTools：工具执行（走公共执行器 runAgentTools，产物节点差异由调用方 hooks 消化），返回 tool 消息由引擎回填下一轮
 *
 * 类型全部用中性词汇（LlmMessage/ToolSchema），工具执行走 services/ai/tools 注册表（runAgentTools）。
 */
import { streamChat, STREAM_IDLE_TIMEOUT_MS } from "@/services/ai/client";
import { isContextOverflowError } from "@/services/ai/errors";
import { planOverflowRetry } from "@/utils/agentHistory";
import { autoTitle, AUTO_NAMING_DELAY_MS } from "@/services/ai/autoTitle";
import { summarizeAgentTool, summarizePartialAgentTool } from "@/services/ai/tools";
import { PENDING_RUN_ID_PREFIX } from "@/constants/chat";
import { useSettingsStore } from "./settingsStore";
import type {
  ChatAutoNameResult,
  ChatNamingTarget,
  ProviderConfig,
  ReasoningEffort,
  ToolSchema,
  ToolExecResult,
  LlmMessage,
  LlmToolCall,
  LlmFinishReason,
  ToolRun,
} from "@/types";

interface StreamBatch {
  content: string;
  reasoning: string;
}

/**
 * 工具调用轮数无预算：只要模型还在调用工具就继续，模型自会在输出不带工具的正文时收束；
 * 失控由 max-tokens（输出上限）+ 用户停止按钮兜底，不设固定轮数上限。
 */
interface RunStreamExchangeOptions {
  provider: ProviderConfig;
  model: string;
  apiMessages: LlmMessage[];
  /** 思考档位：下发 `reasoning_effort` 以开启模型思考（仅对支持思考的模型生效）；缺省 = 不指定。 */
  reasoningEffort?: ReasoningEffort;
  /** 传入 = 启用工具循环（无轮数上限，模型不调工具即收束）；不传 = 单轮。 */
  tools?: ToolSchema[];
  signal: AbortSignal;
  applyBatch: (batch: StreamBatch) => void;
  onError: (err: Error) => void;
  onDone: (result: {
    content: string;
    reasoning: string;
    timedOut: boolean;
    /** 达到输出上限（finish_reason=length）：工具调用不执行（参数可能残缺），调用方附截断提示。 */
    truncated: boolean;
    /**
     * 最终回答轮（本轮无工具调用）：该轮正文经叙述通道流式展示，轮末应提升进 content——
     * 调用方对占位消息 steps 调 `promoteTrailingNarration` 后写入。中止/工具轮/单轮模式为 false。
     */
    promoteNarration: boolean;
  }) => void;
  /**
   * 工具执行（画布/面板均走 services/ai/tools 的 runAgentTools，产物节点差异由调用方 hooks 消化），
   * 返回 tool 消息由引擎回填下一轮，results 为各 tool call 的执行结果摘要（可视化用）。
   */
  executeTools: (
    calls: LlmToolCall[],
  ) => Promise<{ messages: LlmMessage[]; results: ToolExecResult[] }>;
  /** 工具调用过程通知（可视化）：跨轮**全量**累积列表，执行前发 running、执行后发 done/error（调用方 mergeToolRuns 交错进 steps）。 */
  onToolRuns?: (runs: ToolRun[]) => void;
  /**
   * 工具轮的叙述正文增量通知：每轮在调用工具前说的普通文本**边生成边**追加为一个「叙述行」text 步
   * （不是缓冲到轮末，从而保留流式打字效果），不进 content。
   */
  onNarration?: (text: string) => void;
}

export async function runStreamExchange(
  options: RunStreamExchangeOptions,
): Promise<void> {
  let apiMessages = options.apiMessages;

  // 引擎内部 controller：空闲超时用内部 abort；外部 signal（停止按钮/切画布）变化时转发中止。
  // 注意：addEventListener 不会对「调用前已 aborted」的信号回放事件，须显式补一次检查
  const controller = new AbortController();
  if (options.signal.aborted) controller.abort();
  options.signal.addEventListener("abort", () => controller.abort(), {
    once: true,
  });
  const signal = controller.signal;

  // 双通道累积（本地变量与 applyBatch 同步累计；onDone 直接取本地值，不依赖调用方状态读取）
  let totalContent = "";
  let totalReasoning = "";
  let pendingDelta = "";
  let pendingReasoning = "";
  // 工具轮叙述正文增量（rAF 合并，避免每 token 一次 setState；与 content/reasoning 同一条 merge 通道）
  let pendingNarration = "";
  let rafId: number | null = null;
  // 工具调用过程跨轮累积：每轮 onToolRuns 发全量，供调用方 mergeToolRuns 交错进 steps——
  // 否则多轮工具循环只显示最后一轮（调用方整体替换）。
  // **随结算重写为 `let`**：done/中止时回写 allRuns，finally 兜底只能命中「真未回填」的工具，不误标已完成
  let allRuns: ToolRun[] = [];
  // 工具参数流式槽位（当轮，完整调用到达即清）：参数分片边到边累积，随 rAF 帧把
  // 「生成中」工具行叠加在 allRuns 后发出（调用方 mergeToolRuns 按 id 原位刷新）——
  // 长参数（如 write_file 正文）生成阶段不再无任何可见过程
  const pendingArgs = new Map<number, { id: string; name: string; args: string }>();
  let pendingArgsDirty = false;
  // 任一帧发过「生成中」overlay：收尾兜底据此补发一次纯 allRuns，让调用方剪除残留合成行
  let pendingRunsEmitted = false;

  // 收尾兜底：把残留的「生成中」槽位固化为 error 行并入 allRuns（中止路径未经执行轮固化、
  // 无真实行替换；截断路径槽位已由 onToolCalls 清空，此处幂等 no-op）——防 UI 残留转圈行
  const settlePendingArgs = () => {
    if (pendingArgs.size === 0) return;
    const runs: ToolRun[] = [...pendingArgs.values()].map((p) => ({
      id: p.id,
      name: p.name,
      argsSummary: summarizePartialAgentTool(p.name, p.args),
      args: p.args,
      status: "error" as const,
      resultSummary: signal.aborted ? "（已中断）" : "（参数未生成完整）",
    }));
    allRuns.push(...runs);
    pendingArgs.clear();
    pendingArgsDirty = false;
  };

  const applyBatch = (content: string, reasoning: string) => {
    totalContent += content;
    totalReasoning += reasoning;
    options.applyBatch({ content, reasoning });
  };
  const flushPending = () => {
    const d = pendingDelta;
    const r = pendingReasoning;
    const n = pendingNarration;
    pendingDelta = "";
    pendingReasoning = "";
    pendingNarration = "";
    if (n) options.onNarration?.(n);
    if (d || r) applyBatch(d, r);
    // 「生成中」工具行随帧发出：叠加在跨轮已完成列表之后（同 id 行由真实工具行原位替换，
    // 合成 id 行由调用方 mergeToolRuns 的剪除规则随全量列表收敛）
    if (pendingArgsDirty) {
      pendingArgsDirty = false;
      pendingRunsEmitted = true;
      const pendingRuns: ToolRun[] = [...pendingArgs.entries()]
        .sort(([a], [b]) => a - b)
        .map(([, p]) => ({
          id: p.id,
          name: p.name,
          argsSummary: summarizePartialAgentTool(p.name, p.args),
          args: p.args,
          status: "running" as const,
        }));
      options.onToolRuns?.([...allRuns, ...pendingRuns]);
    }
  };
  const scheduleApply = () => {
    if (rafId === null)
      rafId = requestAnimationFrame(() => {
        rafId = null;
        flushPending();
      });
  };
  const cancelRaf = () => {
    if (rafId !== null) {
      cancelAnimationFrame(rafId);
      rafId = null;
    }
  };

  // 空闲超时：每个增量重置，超时无新 token 视为挂起（后端异常/连接半死），自动中止降级
  let timedOut = false;
  let idleTimer: ReturnType<typeof setTimeout> | null = null;
  const resetIdle = () => {
    if (idleTimer) clearTimeout(idleTimer);
    idleTimer = setTimeout(() => {
      idleTimer = null;
      timedOut = true;
      controller.abort();
    }, STREAM_IDLE_TIMEOUT_MS);
  };
  const clearIdle = () => {
    if (idleTimer) {
      clearTimeout(idleTimer);
      idleTimer = null;
    }
  };

  // 无预算工具循环：每轮都携带工具名册（若配置了 Agent），模型不调工具即收束；无工具（单轮模式）只跑一轮。
  const hasTools = !!options.tools?.length;
  let truncated = false;
  let promoteNarration = false;
  // 上下文溢出只折叠重试一次：折叠后仍溢出说明单条结果之外还有别处过长，再重试原样必撞
  let overflowRetried = false;
  // 轮序号：参数分片缺 id 时合成 id 的唯一性来源（跨轮不撞）
  let roundIndex = 0;
  try {
    for (;;) {
      roundIndex++;
      let toolCalls: LlmToolCall[] = [];
      let stopReason: LlmFinishReason | undefined;
      let roundError: Error | null = null;
      // 本轮是否已产出任何增量：溢出重试只在「请求被整轮拒绝」时进行（已产出再重试会重复输出）
      let roundProducedOutput = false;
      // 请求发起即开始空闲计时（首个 token 前的等待也受超时保护）
      resetIdle();
      await streamChat(
        {
          baseUrl: options.provider.baseUrl,
          apiKey: options.provider.apiKey,
          model: options.model,
          ...(options.reasoningEffort ? { reasoningEffort: options.reasoningEffort } : {}),
          messages: apiMessages,
          signal,
          ...(options.tools?.length ? { tools: options.tools } : {}),
          // 传输级重试：网络抖动/5xx/429 自动退避重试（最多 2 次；流开始后不重试，防重复输出）
          retry: { maxRetries: 2 },
        },
        {
          onDelta: (delta) => {
            roundProducedOutput = true;
            // 工具轮里正文是「叙述」（缓冲后 rAF 合并进 text 步，保留流式打字；最终回答轮由调用方
            // 在 onDone promoteNarration 时提升进 content）；无工具（单轮模式）的正文实时进 content。
            if (hasTools) pendingNarration += delta;
            else pendingDelta += delta;
            scheduleApply();
            resetIdle();
          },
          onReasoningDelta: (text) => {
            roundProducedOutput = true;
            pendingReasoning += text;
            resetIdle();
            scheduleApply();
          },
          onToolCalls: (tc) => {
            roundProducedOutput = true;
            toolCalls = tc;
            // 完整调用已到达：把「生成中」行固化为正式 running 行进 allRuns（去重）——
            // 进入执行轮时 runningRuns 同 id 原位替换；截断（max-tokens）路径无执行轮，
            // 由 finally 的 running 归一兜底收口。清槽防轮末 flushPending 再发过期 overlay。
            // 槽位按 wire index 序与完整调用一一对应（两侧同源于 toolCallAcc 的同一批 index）：
            // 分片缺 id 的网关在此回填真实 call id；回填后仍无 id 的槽位不固化，交由执行轮
            // runningRuns 建行（与无 id 网关的既有行为一致，防同一次调用出现重复行/假错误行）
            const keys = [...pendingArgs.keys()].sort((a, b) => a - b);
            for (let i = 0; i < keys.length && i < tc.length; i++) {
              const slot = pendingArgs.get(keys[i]);
              if (slot && tc[i].id) slot.id = tc[i].id;
            }
            for (const p of pendingArgs.values()) {
              if (p.id.startsWith(PENDING_RUN_ID_PREFIX)) continue;
              if (!allRuns.some((r) => r.id === p.id)) {
                allRuns.push({
                  id: p.id,
                  name: p.name,
                  argsSummary: summarizeAgentTool(p.name, p.args),
                  args: p.args,
                  status: "running",
                });
              }
            }
            pendingArgs.clear();
            pendingArgsDirty = false;
          },
          // 参数增量：喂空闲超时看门狗（长参数生成不误判挂起）+ 累积进当轮槽位（随帧刷「生成中」行）
          onToolCallDelta: (delta) => {
            roundProducedOutput = true;
            resetIdle();
            if (!hasTools) return;
            const slot = pendingArgs.get(delta.index);
            if (slot) {
              if (delta.id) slot.id = delta.id;
              if (delta.name) slot.name = delta.name;
              slot.args += delta.argumentsDelta;
            } else {
              pendingArgs.set(delta.index, {
                id: delta.id ?? `${PENDING_RUN_ID_PREFIX}${roundIndex}:${delta.index}`,
                name: delta.name ?? "",
                args: delta.argumentsDelta,
              });
            }
            pendingArgsDirty = true;
            scheduleApply();
          },
          onDone: (sr) => {
            // 清理在循环外统一做（工具轮的占位不能提前移除）
            stopReason = sr;
          },
          onError: (err) => {
            roundError = err;
          },
        },
      );

      // 空闲超时只覆盖「等待 token」阶段：streamChat 一结束立即收掉计时器，
      // 否则计时器贯穿 executeTools——慢工具执行（搜索等）会被误判挂起中止
      clearIdle();

      if (roundError) {
        // 上下文溢出且本轮未产出任何内容：先折叠超长的历史工具结果再试一次（轻量先手，不删消息）。
        // 折叠无变化（没有超长工具结果）或已重试过 → 交回调用方按溢出提示收尾。
        if (!overflowRetried && !roundProducedOutput && isContextOverflowError(roundError)) {
          const plan = planOverflowRetry(apiMessages);
          if (plan.changed) {
            overflowRetried = true;
            apiMessages = plan.messages;
            continue;
          }
        }
        cancelRaf();
        clearIdle();
        options.onError(roundError);
        return;
      }

      // 中止（停止按钮/空闲超时）：不提升、不执行工具——保留已产出叙述为步骤，由调用方 decideCleanup 收尾
      if (signal.aborted) break;

      if (toolCalls.length === 0) {
        // 模型收束：最终回答轮（无工具调用）。正文走了叙述通道（流式打字），
        // 标记 promoteNarration 让调用方在轮末把该轮叙述提升进 content；
        // 若因输出上限截断，truncated 一并告知（调用方附截断提示）。
        truncated = stopReason === "max-tokens";
        promoteNarration = hasTools;
        break;
      }

      // 回复被截断（finish_reason=length → 中性 max-tokens）：tool call 参数可能残缺，不执行工具
      // （防残缺参数执行产生误导产物）；优雅收尾——保留已产出的叙述，截断提示由调用方消费
      if (stopReason === "max-tokens") {
        truncated = true;
        promoteNarration = false;
        break;
      }

      // 执行工具调用（走公共执行器 runAgentTools，画布/面板差异由 executeTools 回调消化）：
      // 用户点停止后，已发出的请求结果回来时被下方 aborted 检查丢弃（不建产物），
      // 下一轮携已 abort 的 signal 立即收敛
      const runningRuns: ToolRun[] = toolCalls.map((tc) => ({
        id: tc.id,
        name: tc.name,
        argsSummary: summarizeAgentTool(tc.name, tc.arguments),
        args: tc.arguments,
        status: "running",
      }));
      // 先 flush 当轮思考与叙述再发工具步：思考/叙述增量经 rAF 异步落 steps，若不同步 flush，
      // 工具步会先于其思考/叙述进入 steps（整条消息「工具一串、思考堆在后」分不清对应哪步）
      cancelRaf();
      flushPending();
      // 跨轮累积后发全量：调用方合并进 steps（多轮思考→工具交错展示）。
      // onToolCalls 已把「生成中」行固化为同 id running 行——此处去重并入防重复
      for (const run of runningRuns) {
        if (!allRuns.some((r) => r.id === run.id)) allRuns.push(run);
      }
      options.onToolRuns?.([...allRuns]);
      const { messages: toolMessages, results } = await options.executeTools(toolCalls);
      // 执行完成：running → done/error + 结果摘要（可视化块实时更新）。
      // **先于中止分支映射**：并行下用户停止时，部分调用可能在中止前已完成并回填（results 含其结果），
      // 须先标 done/error，否则会被下方中止分支误标「（已中断）」。
      // **回写 allRuns**（新对象，引用变化触发气泡重渲染），finally 兜底据此只处理真未回填的工具
      allRuns = allRuns.map((run) => {
        const res = results.find((r) => r.id === run.id);
        if (!res) return run; // 前几轮已完成记录，原样保留
        return {
          ...run,
          status: res.ok ? ("done" as const) : ("error" as const),
          resultSummary: res.summary ?? "完成",
          result: res.detail ?? res.summary,
        };
      });
      options.onToolRuns?.([...allRuns]);
      if (signal.aborted) {
        // 中止：仍 running 的（真未完成/未回填）归一化为「（已中断）」error（否则 running 状态随消息落盘，
        // 画布重开后工具块永久转圈；面板解析侧另有同款归一化兜底）；已完成记录保留
        allRuns = allRuns.map((run) =>
          run.status === "running"
            ? { ...run, status: "error" as const, resultSummary: "（已中断）" }
            : run,
        );
        options.onToolRuns?.([...allRuns]);
        break;
      }
      apiMessages = [
        ...apiMessages,
        // 中性语义：assistant 带 toolCalls 时 text 为 null（适配器转 OpenAI 线时 content 为 null，
        // 部分兼容网关对空串返回 500）
        { role: "assistant", text: null, toolCalls },
        ...toolMessages,
      ];
    }

    // 收尾：固化残留「生成中」槽位（中止/截断路径未经过执行轮），终帧 flush 不再发过期 overlay
    settlePendingArgs();
    cancelRaf();
    clearIdle();
    flushPending();
    options.onDone({
      content: totalContent,
      reasoning: totalReasoning,
      timedOut,
      truncated,
      promoteNarration,
    });
  } catch (e) {
    cancelRaf();
    clearIdle();
    options.onError(e as Error);
  } finally {
    settlePendingArgs();
    // 「生成中」overlay 兜底：发过就补一次纯 allRuns——调用方 mergeToolRuns 剪除不在列表中的
    // 合成行（中止/出错/参数残缺路径统一收敛，不残留永久转圈行）
    if (pendingRunsEmitted) {
      options.onToolRuns?.([...allRuns]);
    }
    // 收尾兜底：任何原因（异常/结果缺失）下仍在 running 的工具行归一到终态，保证 UI 不会永久转圈。
    // 仅在确有 running 残留时才发（正常路径已全部 done，不 re-emit，避免误标已完成工具）
    if (allRuns.some((r) => r.status === "running")) {
      options.onToolRuns?.(
        allRuns.map((run) =>
          run.status === "running"
            ? { ...run, status: "error" as const, resultSummary: "（结果未回填）" }
            : run,
        ),
      );
    }
  }
}

type CleanupDecision =
  { kind: "remove" } | { kind: "timeout-error" } | { kind: "keep" };

/** 结束清理决策（画布/面板共用）：空回复移除占位；超时且回答未产出写超时错误（保留思考）；否则保留。
 *  `hasSteps`：已产出叙述/工具步骤（steps）时即便最终正文为空也保留——agent 的过程不该被整条删掉。 */
export function decideCleanup(
  content: string,
  reasoning: string,
  timedOut: boolean,
  hasSteps = false,
): CleanupDecision {
  if (hasSteps) return { kind: "keep" };
  if (!content.trim() && !reasoning.trim()) {
    return timedOut ? { kind: "timeout-error" } : { kind: "remove" };
  }
  if (!content.trim() && timedOut) return { kind: "timeout-error" };
  return { kind: "keep" };
}

/**
 * 话题命名目标（画布对话节点 / AI 对话面板会话的差异由回调注入，命名管线共用一份）——
 * 契约见 types/chatRuntime.ts 的 `ChatNamingTarget`：回调在延迟后与写回前被重取，
 * 命名期间的新消息纳入摘要、已命名/已删除则不写。
 */

type AutoNamingResult = ChatAutoNameResult;

/**
 * 公共话题命名管线（画布/面板共用，命名经对话核心能力的 autoName 触达）：
 * 解析命名模型（设置指定 → 仓库默认；关闭/未配置返回 skipped）→ 可选延迟（缺省 3s 防限流；
 * 重新命名传 0 立即发出）→ 消息检查（user + assistant 各一）→ LLM 生成（超时 60s /
 * 可选全量历史）→ 二次校验未命名再写回。
 * 返回：ok = 成功写回；skipped = 无模型/无消息/已命名/被主动中止（调用方静默）；
 * failed = LLM 请求失败（调用方提示可重试）。
 * `ignoreToggle` = 重新命名：不受「话题自动命名」开关限制（用户显式请求）。
 */
export async function runAutoNaming(
  target: ChatNamingTarget,
  opts?: { delayMs?: number; maxChars?: number; ignoreToggle?: boolean; key?: string },
): Promise<AutoNamingResult> {
  const named = useSettingsStore.getState().resolveAutoNamingModel(opts?.ignoreToggle);
  if (!named) return "skipped";
  const delay = opts?.delayMs ?? AUTO_NAMING_DELAY_MS;
  if (delay > 0) await new Promise((r) => setTimeout(r, delay));
  if (target.isNamed()) return "skipped";
  const messages = target.getMessages();
  // 至少 user + assistant 各一才有摘要意义（纯 user 未回复/abort 未产出——下轮再试）
  if (
    !messages.some((m) => m.role === "user") ||
    !messages.some((m) => m.role === "assistant")
  ) {
    return "skipped";
  }
  const { provider, model } = named;
  const dialogue = messages
    .map((m) => `${m.role === "user" ? "用户" : "AI"}：${m.displayContent ?? m.content}`)
    .join("\n");
  // 命名不传 temperature：交给厂商 API 默认配置（与主对话一致）；
  // key = 目标标识（会话 id/对话节点 id）：abortAutoTitle(key) 只中止对应目标的命名请求
  const { aborted, title } = await autoTitle(
    { baseUrl: provider.baseUrl, apiKey: provider.apiKey, model },
    dialogue,
    {
      maxChars: opts?.maxChars,
      ...(opts?.key ? { key: opts.key } : {}),
    },
  );
  if (aborted) return "skipped";
  if (!title) return "failed";
  // 命名期间目标可能已命名/已删除 → 校验后再写，防并发覆盖
  if (target.isNamed()) return "skipped";
  target.applyTitle(title);
  return "ok";
}
