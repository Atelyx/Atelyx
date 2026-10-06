/**
 * Agent 步进（`Message.steps`）的纯数据操作：画布与面板共用。
 * `steps` 为展示的权威来源（思考步与工具步按序交错）；旧 `.atlx` 消息只有 `reasoningContent`
 * +`toolRuns` 遗留字段，经 `normalizeAgentSteps` 推导为「单思考步 + 工具步」，新写入一律只写 `steps`。
 * 思考累积与工具轮合并均为不可变更新：只并入当前轮最后一个同类型步（尾部向前找、遇工具步即停），
 * 避免思考/叙述同帧到达时一段思考被拆成两个思考块。
 */
import type { AgentStep, Role, ToolRun } from "@/types";
import { ERROR_PREFIX, TRUNCATED_TEXT, PENDING_RUN_ID_PREFIX } from "@/constants/chat";

/**
 * 思考增量写入 steps：并入当前轮最后一个思考步（尾部向前找第一个 reasoning，遇工具步即停）。
 * 只当最后一步是思考步才拼接会漏掉「思考/叙述同帧到达」的情况——叙述步插在中间后，
 * 后续思考增量会被拆成新思考步（一段思考渲染成两个折叠块）。工具轮之间依旧分隔。
 */
export function appendReasoning(steps: AgentStep[], text: string): AgentStep[] {
  if (!text) return steps;
  for (let i = steps.length - 1; i >= 0; i--) {
    const s = steps[i];
    if (s.kind === "reasoning") {
      return [
        ...steps.slice(0, i),
        { kind: "reasoning", text: s.text + text },
        ...steps.slice(i + 1),
      ];
    }
    if (s.kind === "tool") break;
  }
  return [...steps, { kind: "reasoning", text }];
}

/** 叙述正文增量写入 steps（工具轮的文本叙述，渲染为该步的「叙述行」）：并入当前轮最后一个 text 步（尾部向前找，遇工具步即停），否则新起。 */
export function appendNarration(steps: AgentStep[], text: string): AgentStep[] {
  if (!text) return steps;
  for (let i = steps.length - 1; i >= 0; i--) {
    const s = steps[i];
    if (s.kind === "text") {
      return [
        ...steps.slice(0, i),
        { kind: "text", text: s.text + text },
        ...steps.slice(i + 1),
      ];
    }
    if (s.kind === "tool") break;
  }
  return [...steps, { kind: "text", text }];
}

/**
 * 轮末正文提升：把 steps 末尾**连续**的「叙述行」（text 步）提升为最终回复 content。
 * 无预算工具循环下，最终回答轮无工具调用，其正文以叙述行流式展示在 steps——
 * 轮末需要落进 content 作为最终回复（画布/面板 onDone 在 `promoteNarration` 时调用）。
 * 尾部连续文本步之外（如遇 tool/reasoning 步）不再提升；无尾部文本步时原样返回。
 */
export function promoteTrailingNarration(steps: AgentStep[]): {
  content: string;
  steps: AgentStep[];
} {
  let end = steps.length;
  const chunks: string[] = [];
  while (end > 0 && steps[end - 1].kind === "text") {
    chunks.unshift((steps[end - 1] as Extract<AgentStep, { kind: "text" }>).text);
    end--;
  }
  if (chunks.length === 0) return { content: "", steps };
  return { content: chunks.join("\n"), steps: steps.slice(0, end) };
}

/**
 * onDone 最终化（画布/面板共用）：最终回答轮把尾部叙述提升进 content（`promoteNarration`），
 * 输出上限截断时附 `TRUNCATED_TEXT` 提示（有正文则尾接，正文为空则 `[错误]` 占位）。
 * 返回最终 content 与 steps，供调用方 decideCleanup 后写入占位消息。
 */
export function finalizeReplyText(args: {
  content: string;
  steps: AgentStep[];
  promoteNarration: boolean;
  truncated: boolean;
}): { content: string; steps: AgentStep[] } {
  let finalContent = args.content;
  let finalSteps = args.steps;
  if (args.promoteNarration) {
    const res = promoteTrailingNarration(finalSteps);
    finalSteps = res.steps;
    finalContent = res.content || finalContent;
  }
  if (args.truncated) {
    finalContent = finalContent.trim()
      ? `${finalContent}\n\n${TRUNCATED_TEXT}`
      : `${ERROR_PREFIX} ${TRUNCATED_TEXT}`;
  }
  return { content: finalContent, steps: finalSteps };
}

/**
 * assistant 可回复正文：content 非空直接返回；否则回退到所有 text 叙述步拼接——
 * 未走轮末提升路径的叙述-only 消息（中止/未提升）的复制/API 历史/分支门控均以此为正文兜底。
 */
export function assistantReplyText(msg: {
  content: string;
  steps?: AgentStep[];
}): string {
  if (msg.content) return msg.content;
  return (msg.steps ?? [])
    .filter((s): s is Extract<AgentStep, { kind: "text" }> => s.kind === "text")
    .map((s) => s.text)
    .join("\n");
}

/**
 * assistant 消息 content 为空时以叙述步拼接回填（叙述-only 消息的正文在 steps）：
 * API 历史 / 话题命名等按 content 取正文的消费方共用，防空正文丢失；其余消息原样返回。
 */
export function fillAssistantReplyText<
  T extends { role: Role; content: string; steps?: AgentStep[] },
>(m: T): T {
  return m.role === "assistant" && !m.content
    ? { ...m, content: assistantReplyText(m) }
    : m;
}

/** 参数生成中的合成工具步（id 带 `PENDING_RUN_ID_PREFIX` 前缀，由流式引擎在参数分片缺 id 时合成）。 */
function isPendingSyntheticStep(s: AgentStep): s is Extract<AgentStep, { kind: "tool" }> {
  return s.kind === "tool" && s.run.id.startsWith(PENDING_RUN_ID_PREFIX);
}

/**
 * 工具调用合并进 steps：按 `run.id` 更新已有工具步 / 追加新工具步。
 * 跨工具轮累积全量传入（引擎发全量列表），所以多轮工具全保留、状态实时刷新。
 * 参数生成中的合成行（id 带 `PENDING_RUN_ID_PREFIX` 前缀）只随全量列表在场保留：
 * 轮完成被同 id 真实行原位替换，收尾纯全量发出时不在场即剪除（中止/出错/参数残缺都不残留转圈行）；
 * 仅剪 running 态——已结算的终态合成行（如「已中断」）是历史记录，与真实工具中断行同语义，永久保留。
 */
export function mergeToolRuns(steps: AgentStep[], runs: ToolRun[]): AgentStep[] {
  let changed = false;
  const runIds = new Set(runs.map((r) => r.id));
  const hasPending = steps.some(
    (s) => isPendingSyntheticStep(s) && s.run.status === "running",
  );
  const base = hasPending
    ? steps.filter((s) => {
        if (!isPendingSyntheticStep(s) || s.run.status !== "running") return true;
        const keep = runIds.has(s.run.id);
        if (!keep) changed = true;
        return keep;
      })
    : steps;
  const next = base.map((s) => {
    if (s.kind !== "tool") return s;
    const run = runs.find((r) => r.id === s.run.id);
    if (!run || run === s.run) return s;
    changed = true;
    return { kind: "tool" as const, run };
  });
  const existingIds = new Set(
    next.filter((s): s is Extract<AgentStep, { kind: "tool" }> => s.kind === "tool").map((s) => s.run.id),
  );
  const fresh = runs.filter((r) => !existingIds.has(r.id));
  if (fresh.length === 0) return changed ? next : steps;
  return [...next, ...fresh.map((run) => ({ kind: "tool" as const, run }))];
}

/** 旧消息（无 steps）→ 推导 steps；已含 steps 则原样返回。 */
export function normalizeAgentSteps(msg: {
  steps?: AgentStep[];
  reasoningContent?: string;
  toolRuns?: ToolRun[];
}): AgentStep[] | undefined {
  if (msg.steps && msg.steps.length > 0) return msg.steps;
  if (!msg.steps && !msg.reasoningContent && !msg.toolRuns?.length) return undefined;
  const steps: AgentStep[] = [];
  if (msg.reasoningContent) steps.push({ kind: "reasoning", text: msg.reasoningContent });
  for (const run of msg.toolRuns ?? []) steps.push({ kind: "tool", run });
  return steps;
}

/**
 * 归并旧数据里被拆开的同轮思考/叙述步（幂等）：按 `appendReasoning`/`appendNarration` 的
 * 同轮合并语义重放一遍，把「思考/叙述交错到达」时期落盘的 `[reasoning, text, reasoning]`
 * 这类分裂 steps 愈合为单思考块 + 单叙述行。加载旧消息时调用，新流式不会产生分裂。
 * 参数生成中的合成行仅在 running 态归一为已中断（流式中途落盘的瞬时残留）；
 * 终态合成行是历史记录，原样保留状态与文案，防重开后丢失或被改写。
 */
export function coalesceAgentSteps(steps: AgentStep[]): AgentStep[] {
  return steps.reduce<AgentStep[]>((acc, s) => {
    if (s.kind === "reasoning") return appendReasoning(acc, s.text);
    if (s.kind === "text") return appendNarration(acc, s.text);
    if (isPendingSyntheticStep(s)) {
      return s.run.status === "running"
        ? [
            ...acc,
            {
              kind: "tool" as const,
              run: { ...s.run, status: "error" as const, resultSummary: "（已中断）" },
            },
          ]
        : [...acc, s];
    }
    return [...acc, s];
  }, []);
}

/** 一轮步骤组 = 该步的思考/叙述（按序，多为 reasoning 或 text）+ 其全部工具行（渲染分组用）。 */
export interface AgentStepGroup {
  thinkings: Array<{ kind: "reasoning" | "text"; text: string }>;
  tools: ToolRun[];
}

/**
 * 扁平 steps → 步骤组：一轮的思考/叙述与其后的工具归为一组（最终纯思考自成一组的 thinkings）。
 * 引擎按轮发出 [reasoning?][text?][tool…]，思考/叙述在工具前，此分组才能正确对应「哪步思考→哪步工具」。
 */
export function groupAgentSteps(steps: AgentStep[]): AgentStepGroup[] {
  const groups: AgentStepGroup[] = [];
  for (const step of steps) {
    if (step.kind === "reasoning" || step.kind === "text") {
      const last = groups[groups.length - 1];
      if (last && last.tools.length === 0) {
        last.thinkings = [...last.thinkings, { kind: step.kind, text: step.text }];
      } else {
        groups.push({ thinkings: [{ kind: step.kind, text: step.text }], tools: [] });
      }
    } else {
      const last = groups[groups.length - 1];
      if (last) last.tools.push(step.run);
      else groups.push({ thinkings: [], tools: [step.run] });
    }
  }
  return groups;
}
